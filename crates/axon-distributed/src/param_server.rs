//! Parameter Server 配置

use serde::{Deserialize, Serialize};

/// Parameter Server 配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamServerConfig {
    /// 服务器地址
    pub server_address: String,
    /// 端口
    pub port: u16,
    /// 参数同步间隔（秒）
    pub sync_interval_s: f64,
    /// push/pull 超时（毫秒）
    pub push_pull_timeout_ms: u64,
}

impl ParamServerConfig {
    /// 创建默认配置
    pub fn default_config() -> Self {
        Self {
            server_address: "parameter-server".to_string(),
            port: 8787,
            sync_interval_s: 1.0,
            push_pull_timeout_ms: 5000,
        }
    }

    /// 校验合法性
    pub fn validate(&self) -> Result<(), String> {
        if self.server_address.is_empty() {
            return Err("server_address must not be empty".to_string());
        }
        if self.port == 0 {
            return Err("port must be > 0".to_string());
        }
        if self.sync_interval_s <= 0.0 || !self.sync_interval_s.is_finite() {
            return Err(format!(
                "sync_interval_s ({}) must be > 0",
                self.sync_interval_s
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = ParamServerConfig::default_config();
        assert_eq!(cfg.port, 8787);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_empty_address() {
        let mut cfg = ParamServerConfig::default_config();
        cfg.server_address = String::new();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_zero_port() {
        let mut cfg = ParamServerConfig::default_config();
        cfg.port = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_invalid_interval() {
        let mut cfg = ParamServerConfig::default_config();
        cfg.sync_interval_s = -1.0;
        assert!(cfg.validate().is_err());
    }
}
