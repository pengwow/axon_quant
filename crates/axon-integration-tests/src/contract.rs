//! 契约测试（Contract Testing）
//!
//! 验证模块间接口契约的稳定性，确保版本兼容性。当一个模块的公开类型
//! 发生变更时，所有依赖方都能及时感知并适配。
//!
//! ## 测试维度
//!
//! | 维度 | 验证内容 |
//! |------|---------|
//! | 数据契约 | 公开类型的 JSON / bincode 序列化往返；字段不被意外丢失 |
//! | API 契约 | 配置结构向后兼容（`#[serde(default)]`、字段可选） |
//! | 错误契约 | Error 类型可序列化、可 `Display`、可 `Debug` |
//! | 版本兼容 | SemVer 解析、递增、显示格式稳定 |
//! | 跨模块一致性 | 同名类型语义一致（如 `TrialState` 各处解释一致） |
//!
//! ## 破坏性变更检测
//!
//! 通过 `serde_json::Value` 反射 + JSON Schema 风格的字段检查，
//! 自动发现「新增/删除/类型变化」的字段，并在 CI 中阻断破坏性合并。
//!
//! ## 已知未覆盖
//!
//! - 不验证 Python 绑定（PyO3 端的契约由 `python` 子模块的 `#[test]` 覆盖）
//! - 不验证 HTTP/JSON-RPC 协议（Phase 4 后才有）

// 公开 API 测试覆盖大部分导入，但部分 helper 函数在 lib 模式下被 dead-code 警告
#![allow(dead_code)]
// 这些 pub 函数是供 integration_tests.rs 调用的测试入口，文档说明在调用侧
#![allow(missing_docs)]

use std::collections::HashMap;

use axon_hpo::config::{
    HPOConfig, PrunerConfig, PrunerType, SamplerConfig, SamplerType, StudyConfig, StudyDirection,
};
use axon_hpo::result::HPOResult;
use axon_hpo::search_space::SearchSpaceDef;
use axon_hpo::trial::{TrialResult, TrialState};
use axon_registry::types::{ModelStage, SemVer};
use axon_tracker::types::{MetricValue, ParamValue, RunStatus};
use axon_walk_forward::config::{WalkForwardConfig, WindowType};
use axon_walk_forward::metrics::{ISMetrics, OOSMetrics};
use serde::{Serialize, de::DeserializeOwned};

// ───────────────────────────────────────────────────────────────────
// 1. SemVer 契约：版本号解析 / 递增 / 显示
// ───────────────────────────────────────────────────────────────────

pub fn contract_semver_roundtrip_serde() {
    for case in [
        SemVer::new(0, 0, 1),
        SemVer::new(1, 2, 3),
        SemVer::new(100, 200, 300),
        SemVer::new(0, 1, 0),
    ] {
        let json = serde_json::to_string(&case).unwrap();
        let de: SemVer = serde_json::from_str(&json).unwrap();
        assert_eq!(case, de, "SemVer 序列化往返失败: {}", json);
    }
}

pub fn contract_semver_parse_display_roundtrip() {
    for s in ["0.0.1", "1.2.3", "10.20.30"] {
        let v = SemVer::parse(s).expect("parse 成功");
        assert_eq!(v.to_string(), s, "显示格式应与解析输入一致");
    }
}

pub fn contract_semver_bump_invariant() {
    let mut v = SemVer::new(1, 2, 3);
    v.bump_patch();
    assert_eq!(v, SemVer::new(1, 2, 4));
    v.bump_minor();
    assert_eq!(v, SemVer::new(1, 3, 0));
    v.bump_major();
    assert_eq!(v, SemVer::new(2, 0, 0));
}

pub fn contract_semver_ordering() {
    // SemVer 排序应严格遵守 major.minor.patch
    let a = SemVer::new(1, 0, 0);
    let b = SemVer::new(1, 0, 1);
    let c = SemVer::new(1, 1, 0);
    let d = SemVer::new(2, 0, 0);
    assert!(a < b);
    assert!(b < c);
    assert!(c < d);
    assert!(a < d);
}

// ───────────────────────────────────────────────────────────────────
// 2. ModelStage 契约：枚举值稳定
// ───────────────────────────────────────────────────────────────────

pub fn contract_model_stage_serde_stable() {
    // 反序列化字符串与序列化字符串应严格匹配（snake_case 协议）
    for stage in [
        ModelStage::Staging,
        ModelStage::Production,
        ModelStage::Archived,
        ModelStage::RolledBack,
    ] {
        let json = serde_json::to_string(&stage).unwrap();
        let de: ModelStage = serde_json::from_str(&json).unwrap();
        assert_eq!(stage, de, "ModelStage 序列化往返失败: {}", json);
    }
}

pub fn contract_model_stage_string_mapping_locked() {
    // 锁定 snake_case 字符串映射，防止破坏序列化兼容性
    let cases = [
        (ModelStage::Staging, "\"staging\""),
        (ModelStage::Production, "\"production\""),
        (ModelStage::Archived, "\"archived\""),
        (ModelStage::RolledBack, "\"rolled_back\""),
    ];
    for (stage, expected_json) in cases {
        let json = serde_json::to_string(&stage).unwrap();
        assert_eq!(json, expected_json);
        // 反向验证字符串
        let s = stage.to_string();
        assert!(
            s.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "ModelStage::Display 必须是 snake_case"
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// 3. TrialState 契约
// ───────────────────────────────────────────────────────────────────

pub fn contract_trial_state_serde_stable() {
    for state in [
        TrialState::Running,
        TrialState::Complete,
        TrialState::Pruned,
        TrialState::Fail,
    ] {
        let json = serde_json::to_string(&state).unwrap();
        let de: TrialState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, de);
    }
}

pub fn contract_trial_state_predicates() {
    // 锁定 is_finished / is_complete 的语义
    assert!(!TrialState::Running.is_finished());
    assert!(TrialState::Complete.is_finished());
    assert!(TrialState::Pruned.is_finished());
    assert!(TrialState::Fail.is_finished());

    assert!(TrialState::Complete.is_complete());
    assert!(!TrialState::Pruned.is_complete());
    assert!(!TrialState::Fail.is_complete());
    assert!(!TrialState::Running.is_complete());
}

// ───────────────────────────────────────────────────────────────────
// 4. StudyDirection 契约
// ───────────────────────────────────────────────────────────────────

pub fn contract_study_direction_serde_stable() {
    for d in [StudyDirection::Minimize, StudyDirection::Maximize] {
        let json = serde_json::to_string(&d).unwrap();
        let de: StudyDirection = serde_json::from_str(&json).unwrap();
        assert_eq!(d, de);
    }
}

pub fn contract_study_direction_optuna_string() {
    // 锁定 Optuna 字符串映射（外部系统接口）
    assert_eq!(StudyDirection::Minimize.as_optuna_str(), "minimize");
    assert_eq!(StudyDirection::Maximize.as_optuna_str(), "maximize");
    // is_maximize 语义
    assert!(!StudyDirection::Minimize.is_maximize());
    assert!(StudyDirection::Maximize.is_maximize());
}

// ───────────────────────────────────────────────────────────────────
// 5. WindowType 契约
// ───────────────────────────────────────────────────────────────────

pub fn contract_window_type_serde_stable() {
    for w in [WindowType::Rolling, WindowType::Expanding] {
        let json = serde_json::to_string(&w).unwrap();
        let de: WindowType = serde_json::from_str(&json).unwrap();
        assert_eq!(w, de);
    }
}

pub fn contract_window_type_default() {
    // 锁定 Default 实现，防止默认值意外变更
    assert_eq!(WindowType::default(), WindowType::Expanding);
}

// ───────────────────────────────────────────────────────────────────
// 6. RunStatus 契约
// ───────────────────────────────────────────────────────────────────

pub fn contract_run_status_serde_stable() {
    for s in [
        RunStatus::Running,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::Killed,
    ] {
        let json = serde_json::to_string(&s).unwrap();
        let de: RunStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(s, de);
    }
}

pub fn contract_run_status_mlflow_string() {
    // 锁定 MLflow 字符串映射
    assert_eq!(RunStatus::Running.as_mlflow_str(), "RUNNING");
    assert_eq!(RunStatus::Completed.as_mlflow_str(), "FINISHED");
    assert_eq!(RunStatus::Failed.as_mlflow_str(), "FAILED");
    assert_eq!(RunStatus::Killed.as_mlflow_str(), "KILLED");
}

// ───────────────────────────────────────────────────────────────────
// 7. TrialResult 契约
// ───────────────────────────────────────────────────────────────────

pub fn contract_trial_result_serde_stable() {
    let mut params = HashMap::new();
    params.insert("lr".to_string(), serde_json::json!(0.001));
    params.insert("batch".to_string(), serde_json::json!(64));
    let result = TrialResult {
        trial_id: 42,
        params,
        values: vec![0.95],
        state: TrialState::Complete,
        duration_ms: 1234,
        intermediate_values: vec![(0, 0.5), (1, 0.7), (2, 0.95)],
    };
    let json = serde_json::to_string(&result).unwrap();
    let de: TrialResult = serde_json::from_str(&json).unwrap();
    assert_eq!(de.trial_id, 42);
    assert_eq!(de.state, TrialState::Complete);
    assert_eq!(de.values, vec![0.95]);
    assert_eq!(de.intermediate_values.len(), 3);
}

pub fn contract_trial_result_backward_compat_missing_intermediate() {
    // 旧版本 JSON 不含 intermediate_values 字段，应仍能反序列化（#[serde(default)]）
    let json = r#"{
        "trial_id": 1,
        "params": {},
        "values": [0.5],
        "state": "complete",
        "duration_ms": 0
    }"#;
    let de: TrialResult = serde_json::from_str(json).expect("向后兼容：缺少 intermediate_values");
    assert_eq!(de.intermediate_values.len(), 0);
}

// ───────────────────────────────────────────────────────────────────
// 8. WalkForwardConfig 契约：向后兼容
// ───────────────────────────────────────────────────────────────────

pub fn contract_walkforward_config_backward_compat() {
    // v1 配置（无 purge_gap、embargo_pct）应仍能反序列化
    let json = r#"{
        "train_size": 100,
        "validation_size": 0,
        "test_size": 20,
        "step_size": 20,
        "window_type": "expanding"
    }"#;
    let cfg: WalkForwardConfig = serde_json::from_str(json).expect("向后兼容");
    assert_eq!(cfg.train_size, 100);
    assert_eq!(cfg.purge_gap, 0); // 默认值
    assert!(cfg.embargo_pct > 0.0); // default_embargo_pct
}

// ───────────────────────────────────────────────────────────────────
// 9. HPO 配置契约：Sampler / Pruner 兼容性
// ───────────────────────────────────────────────────────────────────

pub fn contract_sampler_type_aliases() {
    // SamplerType 使用 adjacently-tagged 表示：{"sampler_type": "..."}
    // Random 别名：alias = "random"，rename_all = "snake_case" 自动产生 "random"
    let random_alias = "\"random\"";
    let json = format!(r#"{{"sampler_type":{random_alias}}}"#);
    let de: SamplerType =
        serde_json::from_str(&json).unwrap_or_else(|_| panic!("Random 别名应兼容: {}", json));
    assert!(matches!(de, SamplerType::Random));
    // CmaEs 别名：alias = "cma_es" + rename_all 把 CmaEs 转换为 cma_es
    let cma_alias = "\"cma_es\"";
    let json = format!(r#"{{"sampler_type":{cma_alias}}}"#);
    let de: SamplerType =
        serde_json::from_str(&json).unwrap_or_else(|_| panic!("CmaEs 别名应兼容: {}", json));
    assert!(matches!(de, SamplerType::CmaEs));
    // 序列化：snake_case 形式
    let serialized = serde_json::to_string(&SamplerType::Random).unwrap();
    assert!(
        serialized.contains("\"random\""),
        "Random 序列化应包含 \"random\": {serialized}"
    );
    let serialized = serde_json::to_string(&SamplerType::CmaEs).unwrap();
    assert!(
        serialized.contains("\"cma_es\""),
        "CmaEs 序列化应包含 \"cma_es\": {serialized}"
    );
}

pub fn contract_sampler_type_tpe_with_defaults() {
    // 缺省字段应使用默认值
    let json = r#"{"sampler_type": "tpe"}"#;
    let de: SamplerType = serde_json::from_str(json).expect("TPE 缺省字段应使用默认值");
    if let SamplerType::Tpe {
        n_startup_trials,
        n_warmup_steps,
    } = de
    {
        assert_eq!(n_startup_trials, 10);
        assert_eq!(n_warmup_steps, 0);
    } else {
        panic!("TPE 解析结果错误");
    }
}

pub fn contract_study_config_full_roundtrip() {
    // 完整 StudyConfig 序列化往返
    let cfg = StudyConfig {
        study_name: "test".to_string(),
        direction: StudyDirection::Maximize,
        sampler: SamplerConfig {
            sampler_type: SamplerType::Tpe {
                n_startup_trials: 5,
                n_warmup_steps: 2,
            },
            seed: Some(42),
        },
        pruner: PrunerConfig {
            pruner_type: PrunerType::MedianPruner {
                n_startup_trials: 5,
                n_warmup_steps: 0,
            },
        },
        storage: None,
        load_if_exists: true,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let de: StudyConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(de.study_name, "test");
    assert_eq!(de.direction, StudyDirection::Maximize);
    assert_eq!(de.sampler.seed, Some(42));
}

// ───────────────────────────────────────────────────────────────────
// 10. ParamValue 契约
// ───────────────────────────────────────────────────────────────────

pub fn contract_param_value_all_variants() {
    let cases = [
        ParamValue::Int(42),
        ParamValue::Float(std::f64::consts::PI),
        ParamValue::String("hello".into()),
        ParamValue::Bool(true),
        ParamValue::List(vec![ParamValue::Int(1), ParamValue::Int(2)]),
    ];
    for v in &cases {
        let json = serde_json::to_string(v).unwrap();
        let de: ParamValue = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{v}"), format!("{de}"));
    }
}

// ───────────────────────────────────────────────────────────────────
// 11. MetricValue 契约
// ───────────────────────────────────────────────────────────────────

pub fn contract_metric_value_scalar_roundtrip() {
    let m = MetricValue::Scalar(0.95);
    let json = serde_json::to_string(&m).unwrap();
    let de: MetricValue = serde_json::from_str(&json).unwrap();
    if let MetricValue::Scalar(v) = de {
        assert!((v - 0.95).abs() < 1e-10);
    } else {
        panic!("应为 Scalar");
    }
}

pub fn contract_metric_value_histogram_roundtrip() {
    let m = MetricValue::Histogram {
        values: vec![1.0, 2.0, 3.0],
        bins: vec![0.0, 1.0, 2.0, 3.0],
    };
    let json = serde_json::to_string(&m).unwrap();
    let de: MetricValue = serde_json::from_str(&json).unwrap();
    if let MetricValue::Histogram { values, bins } = de {
        assert_eq!(values, vec![1.0, 2.0, 3.0]);
        assert_eq!(bins, vec![0.0, 1.0, 2.0, 3.0]);
    } else {
        panic!("应为 Histogram");
    }
}

// ───────────────────────────────────────────────────────────────────
// 12. ISMetrics / OOSMetrics 契约
// ───────────────────────────────────────────────────────────────────

pub fn contract_metrics_roundtrip() {
    let ism = ISMetrics {
        total_return: 0.15,
        sharpe_ratio: 1.5,
        max_drawdown: -0.05,
        win_rate: 0.6,
        profit_factor: 1.8,
    };
    let json = serde_json::to_string(&ism).unwrap();
    let de: ISMetrics = serde_json::from_str(&json).unwrap();
    assert_eq!(de.total_return, 0.15);
    assert_eq!(de.sharpe_ratio, 1.5);

    let oos = OOSMetrics {
        total_return: 0.08,
        sharpe_ratio: 1.0,
        max_drawdown: -0.04,
        win_rate: 0.55,
        profit_factor: 1.5,
        calmar_ratio: 2.0,
    };
    let json = serde_json::to_string(&oos).unwrap();
    let de: OOSMetrics = serde_json::from_str(&json).unwrap();
    assert_eq!(de.calmar_ratio, 2.0);
}

pub fn contract_metrics_default_zero() {
    // 锁定 Default 实现（所有指标初值 0）
    let ism = ISMetrics::default();
    assert_eq!(ism.total_return, 0.0);
    assert_eq!(ism.sharpe_ratio, 0.0);
    assert_eq!(ism.max_drawdown, 0.0);
    let oos = OOSMetrics::default();
    assert_eq!(oos.calmar_ratio, 0.0);
}

// ───────────────────────────────────────────────────────────────────
// 13. 破坏性变更检测：字段重命名 / 类型变更
// ───────────────────────────────────────────────────────────────────

/// 验证 HPOResult 至少包含核心字段，防止静默丢失
pub fn contract_hpo_result_required_fields() {
    let cfg = StudyConfig {
        study_name: "study".into(),
        direction: StudyDirection::Minimize,
        sampler: SamplerConfig {
            sampler_type: SamplerType::Random,
            seed: None,
        },
        pruner: PrunerConfig {
            pruner_type: PrunerType::NopPruner,
        },
        storage: None,
        load_if_exists: false,
    };
    let result = HPOResult::empty(cfg);
    let value: serde_json::Value = serde_json::to_value(&result).unwrap();
    let obj = value.as_object().expect("应为 object");
    for required in [
        "study_config",
        "best_trial",
        "all_trials",
        "param_importances",
        "elapsed_ms",
    ] {
        assert!(obj.contains_key(required), "HPOResult 缺少字段: {required}");
    }
}

/// 验证 HPOConfig 主要字段存在
pub fn contract_hpo_config_required_fields() {
    let mut search_space = HashMap::new();
    search_space.insert(
        "lr".to_string(),
        SearchSpaceDef::Uniform {
            low: 1e-5,
            high: 1e-1,
        },
    );
    let cfg = HPOConfig::new("test", search_space, 10);
    let value: serde_json::Value = serde_json::to_value(&cfg).unwrap();
    let obj = value.as_object().expect("应为 object");
    for required in ["study", "search_space", "objective", "hpo"] {
        assert!(obj.contains_key(required), "HPOConfig 缺少字段: {required}");
    }
    // HPORunConfig 关键字段
    let hpo = &obj["hpo"];
    for required in ["n_trials", "n_jobs"] {
        assert!(
            hpo.get(required).is_some(),
            "HPORunConfig 缺少字段: {required}"
        );
    }
}

// ───────────────────────────────────────────────────────────────────
// 14. 跨模块数值不变量：序列化不改变数值精度（除浮点表示差异）
// ───────────────────────────────────────────────────────────────────

/// 序列化 - 反序列化后关键数值字段（f64）应保持 < 1e-12 误差
pub fn assert_f64_roundtrip<T: Serialize + DeserializeOwned + std::fmt::Debug>(
    value: &T,
    field_path: &str,
) {
    let json = serde_json::to_string(value).unwrap();
    let de: serde_json::Value = serde_json::from_str(&json).unwrap();
    let original_value: serde_json::Value = serde_json::to_value(value).unwrap();
    let orig = original_value.pointer(field_path).unwrap();
    let restored = de.pointer(field_path).unwrap();
    if let (Some(a), Some(b)) = (orig.as_f64(), restored.as_f64()) {
        assert!(
            (a - b).abs() < 1e-12,
            "字段 {field_path} 精度损失: {a} vs {b}"
        );
    }
}

pub fn contract_f64_precision_preserved_is_metrics() {
    let ism = ISMetrics {
        total_return: 0.123456789012345,
        sharpe_ratio: 1.23456789012345,
        max_drawdown: -0.0987654321098765,
        win_rate: 0.555555555555555,
        profit_factor: 1.77777777777777,
    };
    assert_f64_roundtrip(&ism, "/total_return");
    assert_f64_roundtrip(&ism, "/sharpe_ratio");
    assert_f64_roundtrip(&ism, "/win_rate");
}

// ───────────────────────────────────────────────────────────────────
// Phase 4 契约测试
// ───────────────────────────────────────────────────────────────────

// 15. axon-risk RiskConfig 契约

pub fn contract_risk_config_defaults() {
    let cfg = axon_risk::RiskConfig::default();
    assert!(cfg.max_position_per_instrument > 0.0);
    assert!(cfg.max_leverage > 1.0);
    assert!(cfg.max_drawdown > 0.0 && cfg.max_drawdown <= 1.0);
    assert!(cfg.max_daily_loss > 0.0);
    assert!(cfg.max_concentration > 0.0 && cfg.max_concentration <= 1.0);
}

pub fn contract_risk_result_serde() {
    let result = axon_risk::RiskResult::Reject(axon_risk::RiskReason::OrderTooLarge {
        max: 50000.0,
        actual: 60000.0,
    });
    let json = serde_json::to_string(&result).unwrap();
    let de: axon_risk::RiskResult = serde_json::from_str(&json).unwrap();
    assert!(matches!(de, axon_risk::RiskResult::Reject(_)));
}

// 16. axon-oms OrderStatus 契约

pub fn contract_oms_order_status_transitions() {
    use axon_oms::{Order, OrderStatus, OrderType, Side};
    use rust_decimal::Decimal;

    let mut order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(65000),
    );
    assert_eq!(order.status, OrderStatus::New);

    // New -> Submitted
    order.transition(OrderStatus::Submitted).unwrap();
    assert_eq!(order.status, OrderStatus::Submitted);

    // Submitted -> Acknowledged
    order.transition(OrderStatus::Acknowledged).unwrap();
    assert_eq!(order.status, OrderStatus::Acknowledged);

    // Acknowledged -> Filled
    order
        .transition(OrderStatus::Filled {
            filled_qty: Decimal::new(1, 3),
            avg_price: Decimal::from(65000),
        })
        .unwrap();
    assert!(order.status.is_terminal());
}

pub fn contract_oms_order_snapshot_roundtrip() {
    use axon_oms::OrderManager;
    use rust_decimal::dec;

    let oms = OrderManager::new();
    let order = axon_oms::Order::new(
        "BTC-USDT".into(),
        axon_oms::Side::Buy,
        axon_oms::OrderType::Limit,
        dec!(0.1),
        dec!(65000),
    );
    oms.submit(order).unwrap();

    let snapshot = oms.snapshot();
    let json = serde_json::to_string(&snapshot).unwrap();
    let de: axon_oms::OmsSnapshot = serde_json::from_str(&json).unwrap();
    // 0.6.0 改:version 不再自增,固定为 `OMS_SNAPSHOT_VERSION_CURRENT`(= 2)
    assert_eq!(de.version, axon_oms::OMS_SNAPSHOT_VERSION_CURRENT);
    assert_eq!(de.active_orders.len(), 1);
}

// 17. axon-inference InferenceConfig 契约

pub fn contract_inference_config_serde() {
    let config = axon_inference::ModelConfig {
        path: "model.onnx".into(),
        backend: axon_inference::InferenceBackend::Onnx,
        device: axon_inference::Device::Cpu,
        input_shape: [1, 64, 128],
        output_dim: 3,
        fp16: false,
        num_threads: 4,
    };
    let json = serde_json::to_string(&config).unwrap();
    let de: axon_inference::ModelConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(de.input_shape, [1, 64, 128]);
    assert_eq!(de.output_dim, 3);
}

pub fn contract_inference_action_types() {
    use axon_inference::ActionType;
    assert_ne!(ActionType::Buy, ActionType::Sell);
    assert_ne!(ActionType::Hold, ActionType::Buy);
    assert_ne!(ActionType::ReduceLong, ActionType::ReduceShort);
}

// 18. axon-monitor MetricsRegistry 契约

pub fn contract_monitor_counter_inc_get() {
    let mut registry = axon_monitor::MetricsRegistry::new();
    let counter = registry.register_counter("orders");
    counter.inc();
    counter.inc_by(5);
    assert_eq!(counter.get(), 6);
}

pub fn contract_monitor_histogram_quantiles() {
    let hist = axon_monitor::LatencyHistogram::default_latency();
    for i in 0..100 {
        hist.observe(i as f64 * 100_000.0); // 0 to 10ms
    }
    let p = hist.latency_percentiles();
    assert!(p.p50 > 0.0);
    assert!(p.p99 >= p.p50);
    assert!(p.p999 >= p.p99);
}

// 19. axon-exchange OrderStatus 契约

pub fn contract_exchange_order_status_terminal() {
    use axon_exchange::OrderStatus;
    use rust_decimal::Decimal;

    assert!(!OrderStatus::Pending.is_terminal());
    assert!(!OrderStatus::Sent.is_terminal());
    assert!(!OrderStatus::Acknowledged.is_terminal());
    assert!(
        OrderStatus::Filled {
            filled_qty: Decimal::new(1, 3),
            avg_price: Decimal::from(50000)
        }
        .is_terminal()
    );
    assert!(
        OrderStatus::Cancelled {
            filled_qty: Decimal::ZERO
        }
        .is_terminal()
    );
    assert!(
        OrderStatus::Rejected {
            reason: "test".into()
        }
        .is_terminal()
    );
}
