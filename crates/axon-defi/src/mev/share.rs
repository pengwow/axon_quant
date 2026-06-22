//! MEV-Share 集成

use serde::{Deserialize, Serialize};

use crate::error::DefiError;

/// MEV-Share 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MevShareConfig {
    /// Flashbots RPC 端点
    pub rpc_url: String,
    /// 签名私钥
    pub signing_key: String,
    /// 最大等待时间（秒）
    pub max_wait_secs: u64,
}

impl Default for MevShareConfig {
    fn default() -> Self {
        Self {
            rpc_url: "https://relay.flashbots.net".into(),
            signing_key: String::new(),
            max_wait_secs: 60,
        }
    }
}

impl MevShareConfig {
    /// 创建新的配置
    pub fn new(rpc_url: String, signing_key: String) -> Self {
        Self {
            rpc_url,
            signing_key,
            max_wait_secs: 60,
        }
    }

    /// 设置最大等待时间
    pub fn with_max_wait_secs(mut self, secs: u64) -> Self {
        self.max_wait_secs = secs;
        self
    }

    /// 验证配置
    pub fn validate(&self) -> Result<(), DefiError> {
        if self.rpc_url.is_empty() {
            return Err(DefiError::ConfigError("RPC URL is empty".into()));
        }
        if self.signing_key.is_empty() {
            return Err(DefiError::ConfigError("Signing key is empty".into()));
        }
        Ok(())
    }
}

/// MEV-Share 客户端
pub struct MevShareClient {
    config: MevShareConfig,
}

impl MevShareClient {
    /// 创建新的客户端
    pub fn new(config: MevShareConfig) -> Self {
        Self { config }
    }

    /// 获取配置
    pub fn config(&self) -> &MevShareConfig {
        &self.config
    }

    /// 提交交易到 MEV-Share
    pub async fn submit_transaction(
        &self,
        to: &str,
        _data: &str,
        _value: &str,
    ) -> Result<String, DefiError> {
        // 验证参数
        if to.is_empty() {
            return Err(DefiError::ConfigError("To address is empty".into()));
        }

        // 模拟提交（实际实现需要调用 Flashbots API）
        let tx_hash = format!("0x{:064x}", 12345);

        Ok(tx_hash)
    }

    /// 查询交易状态
    pub async fn get_status(&self, tx_hash: &str) -> Result<MevStatus, DefiError> {
        if tx_hash.is_empty() {
            return Err(DefiError::ConfigError("Transaction hash is empty".into()));
        }

        // 模拟状态查询
        Ok(MevStatus::Pending)
    }
}

/// MEV 交易状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MevStatus {
    /// 待处理
    Pending,
    /// 已包含在区块中
    Included,
    /// 被 MEV 保护
    Protected,
    /// 失败
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mev_share_config_default() {
        let config = MevShareConfig::default();
        assert_eq!(config.rpc_url, "https://relay.flashbots.net");
        assert_eq!(config.max_wait_secs, 60);
    }

    #[test]
    fn test_mev_share_config_new() {
        let config = MevShareConfig::new("https://custom.rpc".into(), "0xkey".into());
        assert_eq!(config.rpc_url, "https://custom.rpc");
        assert_eq!(config.signing_key, "0xkey");
    }

    #[test]
    fn test_mev_share_config_with_max_wait() {
        let config = MevShareConfig::default().with_max_wait_secs(120);
        assert_eq!(config.max_wait_secs, 120);
    }

    #[test]
    fn test_mev_share_config_validate_ok() {
        let config = MevShareConfig::new("rpc".into(), "key".into());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_mev_share_config_validate_empty_rpc() {
        let config = MevShareConfig::new("".into(), "key".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_mev_share_config_validate_empty_key() {
        let config = MevShareConfig::new("rpc".into(), "".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_mev_share_config_serialization() {
        let config = MevShareConfig::new("rpc".into(), "key".into());
        let json = serde_json::to_string(&config).unwrap();
        let restored: MevShareConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.rpc_url, restored.rpc_url);
    }

    #[test]
    fn test_mev_share_client_creation() {
        let config = MevShareConfig::new("rpc".into(), "key".into());
        let client = MevShareClient::new(config);
        assert_eq!(client.config().rpc_url, "rpc");
    }

    #[tokio::test]
    async fn test_mev_share_client_submit_transaction() {
        let config = MevShareConfig::new("rpc".into(), "key".into());
        let client = MevShareClient::new(config);
        let result = client.submit_transaction("0xtoken", "0xdata", "1000").await;
        assert!(result.is_ok());
        let tx_hash = result.unwrap();
        assert!(tx_hash.starts_with("0x"));
    }

    #[tokio::test]
    async fn test_mev_share_client_submit_empty_to() {
        let config = MevShareConfig::new("rpc".into(), "key".into());
        let client = MevShareClient::new(config);
        let result = client.submit_transaction("", "0xdata", "1000").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mev_share_client_get_status() {
        let config = MevShareConfig::new("rpc".into(), "key".into());
        let client = MevShareClient::new(config);
        let result = client.get_status("0xtxhash").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mev_share_client_get_status_empty_hash() {
        let config = MevShareConfig::new("rpc".into(), "key".into());
        let client = MevShareClient::new(config);
        let result = client.get_status("").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_mev_status_variants() {
        let statuses = [
            MevStatus::Pending,
            MevStatus::Included,
            MevStatus::Protected,
            MevStatus::Failed("error".into()),
        ];
        assert_eq!(statuses.len(), 4);
    }

    #[test]
    fn test_mev_status_serialization() {
        let status = MevStatus::Included;
        let json = serde_json::to_string(&status).unwrap();
        let restored: MevStatus = serde_json::from_str(&json).unwrap();
        assert!(matches!(restored, MevStatus::Included));
    }
}
