//! 端到端测试:axon-hpo 超参优化完整流程
//!
//! ## 5 个测试场景
//!
//! 1. `hpo_config_toml_roundtrip`:TOML 字符串 → HPOConfig → 序列化 → 反序列化 → 字段一致
//! 2. `hpo_search_space_all_types`:6 种参数类型构造 + 校验 + 序列化
//! 3. `hpo_pareto_front_pipeline`:构造 trials → compute_pareto_front → hypervolume → 验证
//! 4. `hpo_config_from_file`:从 TOML 文件加载默认配置 → 验证字段
//! 5. `hpo_multi_objective_config`:多目标配置 → 方向数 + Optuna 字符串
//!
//! 运行:`cargo test -p axon-hpo --test e2e_hpo_pipeline`

use std::collections::HashMap;

use axon_hpo::{
    HPOConfig, ObjectiveDef, SearchSpaceDef, StudyDirection, TrialResult, TrialState,
    compute_pareto_front, dominates,
};

// ── helpers ────────────────────────────────────────────────────────────

fn make_trial(id: i32, values: Vec<f64>, state: TrialState) -> TrialResult {
    let mut params = HashMap::new();
    params.insert("lr".into(), serde_json::json!(0.001));
    TrialResult::new(id, params, values).with_state(state)
}

// ── 1. TOML → HPOConfig → 序列化 → 反序列化 → 字段一致 ────────────────

#[test]
fn hpo_config_toml_roundtrip() {
    let toml_str = r#"
[study]
study_name = "test_study"
direction = "maximize"
storage = "sqlite:///test.db"
load_if_exists = true

[search_space.learning_rate]
type = "log_uniform"
low = 1e-5
high = 1e-2

[search_space.batch_size]
type = "choice"
choices = [32, 64, 128]

[objective]
type = "single"
direction = "maximize"

[hpo]
n_trials = 50
n_jobs = 4
"#;
    let cfg = HPOConfig::from_toml(toml_str).unwrap();

    // 验证字段
    assert_eq!(cfg.study.study_name, "test_study");
    assert!(cfg.study.direction.is_maximize());
    assert_eq!(cfg.n_trials(), 50);
    assert_eq!(cfg.n_jobs(), 4);
    assert_eq!(cfg.search_space.len(), 2);

    // 序列化 → 反序列化 roundtrip
    let json = serde_json::to_string(&cfg).unwrap();
    let restored: HPOConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.study.study_name, "test_study");
    assert_eq!(restored.n_trials(), 50);
}

// ── 2. 6 种参数类型构造 + 校验 + 序列化 ────────────────────────────────

#[test]
fn hpo_search_space_all_types() {
    let defs = vec![
        SearchSpaceDef::uniform(0.0, 1.0),
        SearchSpaceDef::log_uniform(1e-5, 1e-2),
        SearchSpaceDef::int_uniform(0, 100, 1),
        SearchSpaceDef::discrete(vec![0.1, 0.01, 0.001]),
        SearchSpaceDef::choice(vec!["ppo".into(), "sac".into()]),
        SearchSpaceDef::categorical(vec![
            serde_json::json!(32),
            serde_json::json!("cnn"),
            serde_json::json!(true),
        ]),
    ];

    for def in &defs {
        // 全部应校验通过
        assert!(def.validate().is_ok(), "validate failed for {def:?}");

        // 序列化 → 反序列化 roundtrip
        let json = serde_json::to_string(def).unwrap();
        let parsed: SearchSpaceDef = serde_json::from_str(&json).unwrap();
        assert_eq!(*def, parsed);
    }
}

// ── 3. Pareto front: trials → front → hypervolume → 验证 ──────────────

#[test]
fn hpo_pareto_front_pipeline() {
    // 构造 5 个 trial（最小化方向）
    // t1=(1,5) t2=(5,1) t3=(3,3) t4=(4,4) t5=(2,2)
    let trials = vec![
        make_trial(1, vec![1.0, 5.0], TrialState::Complete),
        make_trial(2, vec![5.0, 1.0], TrialState::Complete),
        make_trial(3, vec![3.0, 3.0], TrialState::Complete),
        make_trial(4, vec![4.0, 4.0], TrialState::Complete),
        make_trial(5, vec![2.0, 2.0], TrialState::Complete),
    ];
    let dirs = vec![StudyDirection::Minimize, StudyDirection::Minimize];

    // 计算 Pareto front
    let front = compute_pareto_front(&trials, &dirs).unwrap();

    // t4=(4,4) 被 t3=(3,3) 和 t5=(2,2) 支配
    // t1=(1,5), t2=(5,1), t3=(3,3), t5=(2,2) 互相不支配
    assert!(
        front.len() >= 3,
        "front 应有至少 3 个点,实为 {}",
        front.len()
    );
    assert!(!front.points.iter().any(|p| p.trial_id == 4));

    // hypervolume（参考点 = 各目标最差值 + margin）
    let hv = front.hypervolume(&[6.0, 6.0]).unwrap();
    assert!(hv > 0.0, "hypervolume 应 > 0,实为 {hv}");
}

// ── 4. 从 TOML 文件加载默认配置 ───────────────────────────────────────

#[test]
fn hpo_config_from_file() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("config")
        .join("default_hpo.toml");
    let cfg = HPOConfig::from_toml_file(&path).unwrap();

    assert_eq!(cfg.study.study_name, "axon_rl_ppo_v1");
    assert!(cfg.study.direction.is_maximize());
    assert!(cfg.n_trials() >= 50);
    // 搜索空间应包含关键参数
    assert!(cfg.search_space.contains_key("learning_rate"));
    assert!(cfg.search_space.contains_key("gamma"));
}

// ── 5. 多目标配置 → 方向数 + Optuna 字符串 ─────────────────────────────

#[test]
fn hpo_multi_objective_config() {
    let mut space = HashMap::new();
    space.insert("lr".to_string(), SearchSpaceDef::log_uniform(1e-5, 1e-2));

    let cfg = HPOConfig::new("multi_obj", space, 20)
        .with_multi_objective(vec![StudyDirection::Maximize, StudyDirection::Minimize]);

    match &cfg.objective.objective {
        ObjectiveDef::Multi { directions } => {
            assert_eq!(directions.len(), 2);
            let optuna_dirs = cfg.objective.objective.to_optuna_directions();
            assert_eq!(optuna_dirs, vec!["maximize", "minimize"]);
        }
        other => panic!("expected Multi,实为 {other:?}"),
    }
}

// ── 6. dominates 函数:多方向混合 ──────────────────────────────────────

#[test]
fn hpo_dominates_mixed_directions() {
    // maximize + minimize 混合
    let dirs = vec![StudyDirection::Maximize, StudyDirection::Minimize];

    // a=(10, 1) 支配 b=(5, 5): a 在目标1更大(好),在目标2更小(好)
    assert!(dominates(&[10.0, 1.0], &[5.0, 5.0], &dirs));

    // a=(10, 5) 不支配 b=(5, 1): a 在目标1更大(好),但在目标2更大(差)
    assert!(!dominates(&[10.0, 5.0], &[5.0, 1.0], &dirs));

    // a=(5, 1) 不支配 b=(5, 1): 完全相等
    assert!(!dominates(&[5.0, 1.0], &[5.0, 1.0], &dirs));
}
