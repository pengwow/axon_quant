//! 分布式训练配置定义

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::DistributedError;

/// 集群配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// worker 数量
    pub num_workers: usize,
    /// 每个 worker 分配的 CPU 数
    pub num_cpus_per_worker: usize,
    /// 每个 worker 分配的 GPU 数（支持小数，如 0.5）
    #[serde(default)]
    pub num_gpus_per_worker: f64,
    /// Ray 集群地址（None = 本地，Some("auto") = 自动检测，Some("ray://host:port") = 远程）
    #[serde(default)]
    pub cluster_address: Option<String>,
    /// Object Store 内存（GB）
    pub object_store_memory_gb: f64,
}

impl ClusterConfig {
    /// 创建本地集群配置
    pub fn local(num_workers: usize) -> Self {
        Self {
            num_workers,
            num_cpus_per_worker: 1,
            num_gpus_per_worker: 0.0,
            cluster_address: None,
            object_store_memory_gb: 2.0,
        }
    }

    /// 校验合法性
    pub fn validate(&self) -> Result<(), String> {
        if self.num_workers == 0 {
            return Err("num_workers must be > 0".to_string());
        }
        if self.num_cpus_per_worker == 0 {
            return Err("num_cpus_per_worker must be > 0".to_string());
        }
        if self.object_store_memory_gb <= 0.0 {
            return Err("object_store_memory_gb must be > 0".to_string());
        }
        if self.num_gpus_per_worker < 0.0 || !self.num_gpus_per_worker.is_finite() {
            return Err(format!(
                "num_gpus_per_worker ({}) must be >= 0",
                self.num_gpus_per_worker
            ));
        }
        Ok(())
    }
}

/// 算法配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlgorithmConfig {
    /// 算法名（"PPO" | "SAC" | "DQN" | "IMPALA" | "APE_X"）
    pub algorithm: String,
    /// 框架（"torch" | "tensorflow"）
    #[serde(default = "default_framework")]
    pub framework: String,
    /// 算法超参数
    #[serde(default)]
    pub hparams: HashMap<String, serde_json::Value>,
}

fn default_framework() -> String {
    "torch".to_string()
}

impl AlgorithmConfig {
    /// 校验合法性
    pub fn validate(&self) -> Result<(), String> {
        const ALLOWED: &[&str] = &["PPO", "SAC", "DQN", "IMPALA", "APE_X"];
        if !ALLOWED.contains(&self.algorithm.as_str()) {
            return Err(format!(
                "algorithm ({}) must be one of {:?}",
                self.algorithm, ALLOWED
            ));
        }
        if self.framework != "torch" && self.framework != "tensorflow" {
            return Err(format!(
                "framework ({}) must be 'torch' or 'tensorflow'",
                self.framework
            ));
        }
        Ok(())
    }
}

/// 资源配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceConfig {
    /// 每 worker 环境数
    pub num_envs_per_worker: usize,
    /// 每次采样的步数
    pub rollout_fragment_length: usize,
    /// 训练批大小
    pub train_batch_size: usize,
    /// SGD minibatch 大小
    pub sgd_minibatch_size: usize,
    /// SGD 迭代次数
    pub num_sgd_iter: usize,
    /// 学习率 schedule：[(step, lr), ...]
    #[serde(default)]
    pub lr_schedule: Option<Vec<(usize, f64)>>,
}

impl ResourceConfig {
    /// 校验合法性
    pub fn validate(&self) -> Result<(), String> {
        if self.num_envs_per_worker == 0 {
            return Err("num_envs_per_worker must be > 0".to_string());
        }
        if self.rollout_fragment_length == 0 {
            return Err("rollout_fragment_length must be > 0".to_string());
        }
        if self.train_batch_size == 0 {
            return Err("train_batch_size must be > 0".to_string());
        }
        if self.sgd_minibatch_size == 0 || self.sgd_minibatch_size > self.train_batch_size {
            return Err(format!(
                "sgd_minibatch_size ({}) must be in (0, train_batch_size={}]",
                self.sgd_minibatch_size, self.train_batch_size
            ));
        }
        if self.num_sgd_iter == 0 {
            return Err("num_sgd_iter must be > 0".to_string());
        }
        Ok(())
    }
}

/// 容错配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaultToleranceConfig {
    /// 最大重试次数
    pub max_retries: usize,
    /// Checkpoint 间隔（秒）
    pub checkpoint_interval_s: u64,
    /// Checkpoint 保存目录
    pub checkpoint_dir: String,
    /// 训练结束时是否保存 checkpoint
    #[serde(default)]
    pub checkpoint_at_end: bool,
    /// 保留 checkpoint 数量
    pub keep_checkpoints_num: usize,
    /// 是否从 checkpoint 恢复
    #[serde(default)]
    pub restore: bool,
}

impl FaultToleranceConfig {
    /// 校验合法性
    pub fn validate(&self) -> Result<(), String> {
        if self.checkpoint_interval_s == 0 {
            return Err("checkpoint_interval_s must be > 0".to_string());
        }
        if self.checkpoint_dir.is_empty() {
            return Err("checkpoint_dir must not be empty".to_string());
        }
        Ok(())
    }
}

/// 分布式训练总配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistributedConfig {
    /// 集群配置
    pub cluster: ClusterConfig,
    /// 算法配置
    pub algorithm: AlgorithmConfig,
    /// 资源配置
    pub resources: ResourceConfig,
    /// 容错配置
    pub fault_tolerance: FaultToleranceConfig,
}

impl DistributedConfig {
    /// 从 TOML 文件加载
    pub fn from_toml_file(path: &std::path::Path) -> Result<Self, DistributedError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| DistributedError::Io(e.to_string()))?;
        Self::from_toml(&content)
    }

    /// 从 TOML 字符串加载
    pub fn from_toml(content: &str) -> Result<Self, DistributedError> {
        let cfg: DistributedConfig =
            toml::from_str(content).map_err(|e| DistributedError::Toml(e.to_string()))?;
        cfg.validate().map_err(DistributedError::Validation)?;
        Ok(cfg)
    }

    /// 校验所有子配置
    pub fn validate(&self) -> Result<(), String> {
        self.cluster.validate()?;
        self.algorithm.validate()?;
        self.resources.validate()?;
        self.fault_tolerance.validate()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_config_local() {
        let cfg = ClusterConfig::local(4);
        assert_eq!(cfg.num_workers, 4);
        assert_eq!(cfg.cluster_address, None);
    }

    #[test]
    fn test_cluster_config_validate_zero_workers() {
        let cfg = ClusterConfig::local(0);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_cluster_config_validate_invalid_gpus() {
        let cfg = ClusterConfig {
            num_workers: 4,
            num_cpus_per_worker: 1,
            num_gpus_per_worker: -0.5,
            cluster_address: None,
            object_store_memory_gb: 2.0,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_algorithm_config_validate_invalid_algo() {
        let cfg = AlgorithmConfig {
            algorithm: "INVALID".to_string(),
            framework: "torch".to_string(),
            hparams: HashMap::new(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_algorithm_config_validate_invalid_framework() {
        let cfg = AlgorithmConfig {
            algorithm: "PPO".to_string(),
            framework: "jax".to_string(),
            hparams: HashMap::new(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_resource_config_validate_ok() {
        let cfg = ResourceConfig {
            num_envs_per_worker: 4,
            rollout_fragment_length: 200,
            train_batch_size: 4000,
            sgd_minibatch_size: 128,
            num_sgd_iter: 10,
            lr_schedule: None,
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_resource_config_validate_minibatch_too_large() {
        let cfg = ResourceConfig {
            num_envs_per_worker: 4,
            rollout_fragment_length: 200,
            train_batch_size: 1000,
            sgd_minibatch_size: 2000,
            num_sgd_iter: 10,
            lr_schedule: None,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_fault_tolerance_config_validate_empty_dir() {
        let cfg = FaultToleranceConfig {
            max_retries: 3,
            checkpoint_interval_s: 300,
            checkpoint_dir: String::new(),
            checkpoint_at_end: true,
            keep_checkpoints_num: 5,
            restore: true,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_distributed_config_from_toml_ok() {
        let toml_content = r#"
[cluster]
num_workers = 4
num_cpus_per_worker = 2
object_store_memory_gb = 4.0

[algorithm]
algorithm = "PPO"
framework = "torch"

[algorithm.hparams]
lr = 3e-4

[resources]
num_envs_per_worker = 4
rollout_fragment_length = 200
train_batch_size = 4000
sgd_minibatch_size = 128
num_sgd_iter = 10

[fault_tolerance]
max_retries = 3
checkpoint_interval_s = 300
checkpoint_dir = "checkpoints/"
keep_checkpoints_num = 5
"#;
        let cfg = DistributedConfig::from_toml(toml_content).expect("parse");
        assert_eq!(cfg.cluster.num_workers, 4);
        assert_eq!(cfg.algorithm.algorithm, "PPO");
        assert_eq!(cfg.resources.train_batch_size, 4000);
    }

    #[test]
    fn test_distributed_config_from_toml_missing_section() {
        let toml_content = r#"
[cluster]
num_workers = 4
"#;
        let result = DistributedConfig::from_toml(toml_content);
        assert!(result.is_err());
    }
}
