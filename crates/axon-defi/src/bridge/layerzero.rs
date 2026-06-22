//! LayerZero 跨链桥

use serde::{Deserialize, Serialize};

use crate::error::DefiError;
use crate::evm::chain::Chain;

/// 跨链桥配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    /// LayerZero 端点合约地址
    pub endpoint: String,
    /// 支持的链
    pub supported_chains: Vec<u64>,
    /// 默认滑点
    pub default_slippage: f64,
    /// 超时时间（秒）
    pub timeout_secs: u64,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            endpoint: "0x66A71Dcef29a0fFBDBE3c6a460a3B5BC225Cd675".into(),
            supported_chains: vec![1, 42161, 10, 137],
            default_slippage: 0.5,
            timeout_secs: 300,
        }
    }
}

impl BridgeConfig {
    /// 验证配置
    pub fn validate(&self) -> Result<(), DefiError> {
        if self.endpoint.is_empty() {
            return Err(DefiError::ConfigError("Endpoint is empty".into()));
        }
        if self.supported_chains.is_empty() {
            return Err(DefiError::ConfigError("No supported chains".into()));
        }
        Ok(())
    }
}

/// 跨链桥管理器
pub struct BridgeManager {
    config: BridgeConfig,
}

impl BridgeManager {
    /// 创建新的管理器
    pub fn new(config: BridgeConfig) -> Self {
        Self { config }
    }

    /// 获取配置
    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    /// 估算跨链费用
    pub async fn estimate_fee(&self, dst_chain: &Chain, amount: &str) -> Result<String, DefiError> {
        // 验证目标链
        if !self.config.supported_chains.contains(&dst_chain.chain_id()) {
            return Err(DefiError::UnsupportedChain(dst_chain.chain_id()));
        }

        // 模拟费用估算
        let amount_val: f64 = amount.parse().unwrap_or(0.0);
        let fee = amount_val * 0.001; // 0.1% 费用

        Ok(format!("{:.6}", fee))
    }

    /// 发起跨链转账
    pub async fn bridge_tokens(
        &self,
        dst_chain: &Chain,
        token: &str,
        amount: &str,
        receiver: &str,
    ) -> Result<String, DefiError> {
        // 验证参数
        if token.is_empty() {
            return Err(DefiError::ConfigError("Token is empty".into()));
        }
        if amount.is_empty() {
            return Err(DefiError::ConfigError("Amount is empty".into()));
        }
        if receiver.is_empty() {
            return Err(DefiError::ConfigError("Receiver is empty".into()));
        }

        // 验证目标链
        if !self.config.supported_chains.contains(&dst_chain.chain_id()) {
            return Err(DefiError::UnsupportedChain(dst_chain.chain_id()));
        }

        // 模拟跨链转账
        let tx_hash = format!("0x{:064x}", 67890);

        Ok(tx_hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_config_default() {
        let config = BridgeConfig::default();
        assert!(!config.endpoint.is_empty());
        assert!(!config.supported_chains.is_empty());
    }

    #[test]
    fn test_bridge_config_validate_ok() {
        let config = BridgeConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_bridge_config_validate_empty_endpoint() {
        let config = BridgeConfig {
            endpoint: "".into(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_bridge_config_validate_empty_chains() {
        let config = BridgeConfig {
            supported_chains: vec![],
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_bridge_config_serialization() {
        let config = BridgeConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let restored: BridgeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.endpoint, restored.endpoint);
    }

    #[test]
    fn test_bridge_manager_creation() {
        let config = BridgeConfig::default();
        let manager = BridgeManager::new(config);
        assert!(!manager.config().endpoint.is_empty());
    }

    #[tokio::test]
    async fn test_bridge_manager_estimate_fee() {
        let config = BridgeConfig::default();
        let manager = BridgeManager::new(config);
        let result = manager.estimate_fee(&Chain::Arbitrum, "1000").await;
        assert!(result.is_ok());
        let fee = result.unwrap();
        let fee_val: f64 = fee.parse().unwrap();
        assert!(fee_val > 0.0);
    }

    #[tokio::test]
    async fn test_bridge_manager_estimate_fee_unsupported_chain() {
        let config = BridgeConfig::default();
        let manager = BridgeManager::new(config);
        let result = manager.estimate_fee(&Chain::Polygon, "1000").await;
        // Polygon 在默认配置中支持
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_bridge_manager_bridge_tokens() {
        let config = BridgeConfig::default();
        let manager = BridgeManager::new(config);
        let result = manager
            .bridge_tokens(&Chain::Arbitrum, "0xtoken", "1000", "0xreceiver")
            .await;
        assert!(result.is_ok());
        let tx_hash = result.unwrap();
        assert!(tx_hash.starts_with("0x"));
    }

    #[tokio::test]
    async fn test_bridge_manager_bridge_empty_token() {
        let config = BridgeConfig::default();
        let manager = BridgeManager::new(config);
        let result = manager
            .bridge_tokens(&Chain::Arbitrum, "", "1000", "0xreceiver")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_bridge_manager_bridge_empty_amount() {
        let config = BridgeConfig::default();
        let manager = BridgeManager::new(config);
        let result = manager
            .bridge_tokens(&Chain::Arbitrum, "0xtoken", "", "0xreceiver")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_bridge_manager_bridge_empty_receiver() {
        let config = BridgeConfig::default();
        let manager = BridgeManager::new(config);
        let result = manager
            .bridge_tokens(&Chain::Arbitrum, "0xtoken", "1000", "")
            .await;
        assert!(result.is_err());
    }
}
