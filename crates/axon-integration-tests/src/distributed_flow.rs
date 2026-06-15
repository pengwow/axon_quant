//! 场景 6：分布式训练全流程
//!
//! 验证：checkpoint 序列化/反序列化 → 配置校验 → 指标序列化

use axon_distributed::checkpoint::{StepMetrics, TrainingCheckpoint};
use axon_distributed::config::DistributedConfig;

/// 场景 6.2: 序列化指标
pub fn run_metrics_serialization() {
    let metrics = StepMetrics {
        step: 100,
        episode_reward_mean: 1.5,
        episode_len_mean: 200.0,
        policy_loss: 0.01,
        value_loss: 0.05,
        entropy: 0.3,
        fps: 1000.0,
    };
    let json = serde_json::to_string(&metrics).unwrap();
    assert!(json.contains("\"step\":100"));
    let decoded: StepMetrics = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.step, 100);
    assert!((decoded.episode_reward_mean - 1.5).abs() < 1e-9);
}

/// 场景 6.3: 保存/加载 checkpoint
pub fn run_checkpoint_save_load() {
    let mut ckpt = TrainingCheckpoint::new(10, vec![0u8; 1024], vec![0u8; 512], vec![0u8; 256]);
    ckpt.add_metrics(StepMetrics {
        step: 10,
        episode_reward_mean: 2.0,
        episode_len_mean: 150.0,
        policy_loss: 0.02,
        value_loss: 0.03,
        entropy: 0.4,
        fps: 800.0,
    });
    // 序列化
    let json = ckpt.to_json().unwrap();
    assert!(json.contains("\"iteration\":10"));
    // 反序列化
    let restored = TrainingCheckpoint::from_json(&json).unwrap();
    assert_eq!(restored.iteration, 10);
    assert_eq!(restored.policy_state.len(), 1024);
    assert_eq!(restored.metrics_history.len(), 1);
    assert!((restored.metrics_history[0].episode_reward_mean - 2.0).abs() < 1e-9);
    // 大小估算
    assert!(restored.size_bytes() > 0);
}

/// 配置校验
pub fn run_config_validation() {
    let toml_str = r#"
[cluster]
num_workers = 4
num_cpus_per_worker = 2
num_gpus_per_worker = 0.0
object_store_memory_gb = 1.0

[algorithm]
algorithm = "PPO"
framework = "torch"

[resources]
num_envs_per_worker = 1
rollout_fragment_length = 200
train_batch_size = 4096
sgd_minibatch_size = 256
num_sgd_iter = 10

[fault_tolerance]
max_retries = 3
checkpoint_interval_s = 300
checkpoint_dir = "/tmp/axon_ckpt"
keep_checkpoints_num = 5
"#;
    let config = DistributedConfig::from_toml(toml_str);
    assert!(config.is_ok(), "合法配置应通过校验: {:?}", config.err());
}

/// 非法配置应报错
pub fn run_invalid_config_rejected() {
    let toml_str = r#"
[cluster]
num_workers = 0
num_cpus_per_worker = 2
num_gpus_per_worker = 0.0
object_store_memory_gb = 1.0

[algorithm]
algorithm = "PPO"
framework = "torch"

[resources]
num_envs_per_worker = 1
rollout_fragment_length = 200
train_batch_size = 4096
sgd_minibatch_size = 256
num_sgd_iter = 10

[fault_tolerance]
max_retries = 3
checkpoint_interval_s = 300
checkpoint_dir = "/tmp/axon_ckpt"
keep_checkpoints_num = 5
"#;
    let config = DistributedConfig::from_toml(toml_str);
    assert!(config.is_err(), "num_workers=0 应报错");
}
