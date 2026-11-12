//! Tracker + Registry 集成测试
//!
//! 验证：Tracker 记录的训练指标驱动 Registry 阶段转换
//! 1. 用 Tracker 记录多轮训练的指标
//! 2. 根据追踪到的指标（最终 loss / sharpe）决策是否提升到 Production
//! 3. 验证决策逻辑与 Registry 实际状态一致

use std::sync::Arc;

use axon_registry::storage::LocalStorage;
use axon_registry::{ModelMetadata, ModelRegistry, ModelStage};
use axon_tracker::ExperimentTracker;
use axon_tracker::backends::MemoryTracker;
use axon_tracker::types::{MetricValue, ParamValue};

/// 提取 MetricValue::Scalar 的 f64
#[allow(dead_code)]
fn scalar(m: &axon_tracker::types::MetricEntry) -> f64 {
    match &m.value {
        MetricValue::Scalar(v) => *v,
        _ => panic!("expected Scalar metric value, got {:?}", m.value),
    }
}

/// 模拟：tracker 记录的训练指标 → 决策逻辑
///
/// 决策规则：
/// - final_sharpe >= 1.5 && final_loss <= 0.3 → 提升到 Production
/// - final_sharpe >= 1.0 → Staging
/// - 其他 → 保持 Staging，不提升
#[allow(dead_code)]
fn should_promote_to_production(final_sharpe: f64, final_loss: f64) -> bool {
    final_sharpe >= 1.5 && final_loss <= 0.3
}

/// 测试：Tracker 指标决策 Registry 阶段转换
pub async fn test_tracker_metrics_drive_promotion() {
    let tmp = tempfile::tempdir().unwrap();
    let tracker = MemoryTracker::new();
    let storage =
        Arc::new(LocalStorage::new(tmp.path().join("models")).expect("create local storage"));
    let registry = ModelRegistry::new(storage);

    // 训练 3 个模型，tracker 记录各自的指标
    let models = [
        ("model_a", 1.2, 0.45), // sharpe=1.2, loss=0.45 → Staging
        ("model_b", 1.8, 0.25), // sharpe=1.8, loss=0.25 → Production
        ("model_c", 0.9, 0.50), // sharpe=0.9, loss=0.50 → Staging
    ];

    for (name, sharpe, loss) in models.iter() {
        // 模拟训练过程：50 步指标记录
        for step in 0..50 {
            let progress = step as f64 / 50.0;
            let current_sharpe = sharpe * progress;
            let current_loss = loss * (1.0 - progress * 0.5);
            tracker
                .log_metric(&format!("{name}_sharpe"), current_sharpe, step)
                .unwrap();
            tracker
                .log_metric(&format!("{name}_loss"), current_loss, step)
                .unwrap();
        }
        // 记录最终指标
        tracker
            .log_metric(&format!("{name}_final_sharpe"), *sharpe, 50)
            .unwrap();
        tracker
            .log_metric(&format!("{name}_final_loss"), *loss, 50)
            .unwrap();
    }

    // 根据 tracker 数据决策 + 注册 + 阶段转换
    for (name, sharpe, loss) in models.iter() {
        let promoted = should_promote_to_production(*sharpe, *loss);

        // 写入注册表
        let artifact_path = tmp.path().join(format!("{name}.bin"));
        std::fs::write(&artifact_path, format!("weights for {name}").as_bytes()).unwrap();

        let mut metrics = std::collections::HashMap::new();
        metrics.insert("final_sharpe".to_string(), *sharpe);
        metrics.insert("final_loss".to_string(), *loss);

        let metadata = ModelMetadata {
            description: format!("Model {name} with sharpe={sharpe}, loss={loss}"),
            metrics,
            ..Default::default()
        };

        let mv = registry
            .register(name, &artifact_path, metadata, None)
            .await
            .unwrap();

        if promoted {
            registry
                .transition_stage(name, &mv.version, ModelStage::Production)
                .await
                .unwrap();
        }
    }

    // 验证：model_b 在 Production，model_a 和 model_c 在 Staging
    let prod_b = registry.get_production("model_b").await.unwrap();
    assert_eq!(prod_b.stage, ModelStage::Production);

    // model_a 和 model_c 没有 Production 版本
    assert!(registry.get_production("model_a").await.is_err());
    assert!(registry.get_production("model_c").await.is_err());

    // 验证 tracker 记录与决策一致
    let b_final_sharpe = tracker.get_metrics_by_key("model_b_final_sharpe");
    assert_eq!(b_final_sharpe.len(), 1);
    assert!((scalar(&b_final_sharpe[0]) - 1.8).abs() < 1e-9);

    println!("[Tracker+Registry] model_b promoted to Production (sharpe=1.8, loss=0.25)");
}

/// 测试：Tracker 记录 + Registry 元数据一致性
pub async fn test_tracker_registry_metadata_consistency() {
    let tmp = tempfile::tempdir().unwrap();
    let tracker = MemoryTracker::new();
    let storage =
        Arc::new(LocalStorage::new(tmp.path().join("models")).expect("create local storage"));
    let registry = ModelRegistry::new(storage);

    // 训练一个模型
    let artifact = tmp.path().join("model.bin");
    std::fs::write(&artifact, b"test weights").unwrap();

    // Tracker 记录超参
    tracker.log_param("lr", &ParamValue::Float(0.001)).unwrap();
    tracker
        .log_param("batch_size", &ParamValue::Int(64))
        .unwrap();
    tracker.log_param("epochs", &ParamValue::Int(100)).unwrap();

    // 训练过程
    for step in 0..100 {
        let loss = (-(step as f64) * 0.01).exp() * 0.5;
        tracker.log_metric("train_loss", loss, step).unwrap();
        if step % 10 == 0 {
            tracker.log_metric("val_loss", loss * 1.1, step).unwrap();
        }
    }

    // 把 tracker 记录的指标 + 参数同步到 registry
    let recorded_params = tracker.get_all_params();
    let final_metrics = tracker.get_metrics_by_key("train_loss");
    let final_loss = final_metrics.last().map(scalar).unwrap_or(0.0);

    let mut metadata_metrics = std::collections::HashMap::new();
    metadata_metrics.insert("final_train_loss".to_string(), final_loss);
    metadata_metrics.insert("total_steps".to_string(), final_metrics.len() as f64);

    let metadata = ModelMetadata {
        description: "Model trained with tracked hyperparameters".to_string(),
        hyperparameters: recorded_params
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::json!(v.to_string())))
            .collect(),
        metrics: metadata_metrics,
        training_duration_secs: Some(120.0),
        ..Default::default()
    };

    let mv = registry
        .register("consistent-model", &artifact, metadata, None)
        .await
        .unwrap();

    // 验证 metadata 与 tracker 一致
    assert_eq!(mv.metadata.hyperparameters.len(), 3);
    assert!(mv.metadata.metrics.contains_key("final_train_loss"));
    assert!(mv.metadata.metrics.contains_key("total_steps"));
    assert_eq!(mv.metadata.metrics.get("total_steps").copied(), Some(100.0));

    // 验证 hash 字段已填充
    assert!(
        !mv.artifact_hash.is_empty(),
        "artifact_hash should be SHA-256"
    );

    println!(
        "[Tracker+Registry] metadata consistent: {} hyperparams, {} metrics",
        mv.metadata.hyperparameters.len(),
        mv.metadata.metrics.len()
    );
}

/// 测试：Tracker flush 不会影响 Registry 状态
pub async fn test_tracker_flush_independent_from_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let tracker = MemoryTracker::new();
    let storage =
        Arc::new(LocalStorage::new(tmp.path().join("models")).expect("create local storage"));
    let registry = ModelRegistry::new(storage);

    let artifact = tmp.path().join("a.bin");
    std::fs::write(&artifact, b"a").unwrap();

    // 记录 10 个指标
    for i in 0..10 {
        tracker.log_metric("loss", 1.0 / (i + 1) as f64, i).unwrap();
    }

    // 注册一个版本
    let mv = registry
        .register(
            "test",
            &artifact,
            ModelMetadata {
                description: "test".to_string(),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

    // tracker.flush() 不应影响 registry 状态
    tracker.flush().unwrap();
    let prod = registry.get_production("test").await;
    assert!(
        prod.is_err(),
        "no production yet, flush shouldn't change this"
    );

    let staged = registry
        .list_versions("test", &Default::default())
        .await
        .unwrap();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].stage, ModelStage::Staging);
    assert_eq!(staged[0].version, mv.version);
}

/// 测试：get(name, None) 取最新版本且不限 stage（Python 绑定 get_latest 的语义契约）
///
/// Python 绑定层的 `ModelRegistry.get_latest` 直接透传 `registry.get(name, None)`，
/// 此处守护该透传的行为：未晋升 Production 的 Staging 记录也能查到，
/// 名字不存在时按 ModelNotFound 报错。
pub async fn test_get_latest_returns_unpromoted_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let storage =
        Arc::new(LocalStorage::new(tmp.path().join("models")).expect("create local storage"));
    let registry = ModelRegistry::new(storage);

    // 注册两个版本，全程不晋升，停留在 Staging
    let artifact = tmp.path().join("latest.bin");
    std::fs::write(&artifact, b"weights").unwrap();
    registry
        .register(
            "latest-model",
            &artifact,
            ModelMetadata {
                description: "v1".to_string(),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let v2 = registry
        .register(
            "latest-model",
            &artifact,
            ModelMetadata {
                description: "v2".to_string(),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

    // get(None) 返回最新版本，即使从未晋升 Production
    let latest = registry.get("latest-model", None).await.unwrap();
    assert_eq!(latest.version, v2.version);
    assert_eq!(latest.stage, ModelStage::Staging);

    // 对照：get_production 查不到未晋升的模型
    assert!(registry.get_production("latest-model").await.is_err());

    // 不存在的名字报 ModelNotFound
    match registry.get("no-such-model", None).await {
        Err(axon_registry::RegistryError::ModelNotFound(name)) => {
            assert_eq!(name, "no-such-model");
        }
        other => panic!("expected ModelNotFound, got {:?}", other.map(|_| ())),
    }

    println!(
        "[Tracker+Registry] get latest of latest-model -> v{} (Staging, production miss as expected)",
        latest.version
    );
}

/// 测试：unpromote 把当前 Production 版本还原回 Staging（Python 绑定 unpromote 的语义契约）
///
/// Python 绑定层的 `ModelRegistry.unpromote` 直接透传 `registry.unpromote(name)`，
/// 此处守护该接线行为：晋升后取消，stage 变回 Staging，且不再有生产版本。
pub async fn test_unpromote_returns_to_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let storage =
        Arc::new(LocalStorage::new(tmp.path().join("models")).expect("create local storage"));
    let registry = ModelRegistry::new(storage);

    let artifact = tmp.path().join("unpromote.bin");
    std::fs::write(&artifact, b"weights").unwrap();
    let mv = registry
        .register(
            "unpromote-model",
            &artifact,
            ModelMetadata {
                description: "v1".to_string(),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    registry
        .transition_stage("unpromote-model", &mv.version, ModelStage::Production)
        .await
        .unwrap();

    let unp = registry.unpromote("unpromote-model").await.unwrap();
    assert_eq!(unp.version, mv.version);
    assert_eq!(unp.stage, ModelStage::Staging);

    // 生产位已空，版本回到待验证态
    assert!(registry.get_production("unpromote-model").await.is_err());

    // 无生产版本时 unpromote 直接报错
    match registry.unpromote("unpromote-model").await {
        Err(axon_registry::RegistryError::NoProductionVersion(name)) => {
            assert_eq!(name, "unpromote-model");
        }
        other => panic!("expected NoProductionVersion, got {:?}", other.map(|_| ())),
    }

    println!(
        "[Tracker+Registry] unpromote of unpromote-model -> v{} (back to Staging, production miss as expected)",
        unp.version
    );
}
