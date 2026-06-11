//! Actor 模型配置（远程环境 Worker）

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Actor 模型配置（远程环境 Worker）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActorConfig {
    /// Actor ID
    pub actor_id: usize,
    /// 环境名称
    pub env_name: String,
    /// 环境配置
    pub env_config: HashMap<String, serde_json::Value>,
    /// 并行环境数
    pub num_envs: usize,
    /// 观测空间形状
    pub observation_space_shape: Vec<usize>,
    /// 动作空间形状
    pub action_space_shape: Vec<usize>,
}

impl ActorConfig {
    /// 校验合法性
    pub fn validate(&self) -> Result<(), String> {
        if self.env_name.is_empty() {
            return Err("env_name must not be empty".to_string());
        }
        if self.num_envs == 0 {
            return Err("num_envs must be > 0".to_string());
        }
        if self.observation_space_shape.is_empty() {
            return Err("observation_space_shape must not be empty".to_string());
        }
        if self.action_space_shape.is_empty() {
            return Err("action_space_shape must not be empty".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ActorConfig {
        ActorConfig {
            actor_id: 0,
            env_name: "AxonTradingEnv".to_string(),
            env_config: HashMap::new(),
            num_envs: 4,
            observation_space_shape: vec![10, 60],
            action_space_shape: vec![1],
        }
    }

    #[test]
    fn test_actor_config_validate_ok() {
        assert!(sample().validate().is_ok());
    }

    #[test]
    fn test_actor_config_empty_env_name() {
        let mut cfg = sample();
        cfg.env_name = String::new();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_actor_config_zero_envs() {
        let mut cfg = sample();
        cfg.num_envs = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_actor_config_empty_obs_shape() {
        let mut cfg = sample();
        cfg.observation_space_shape = vec![];
        assert!(cfg.validate().is_err());
    }
}
