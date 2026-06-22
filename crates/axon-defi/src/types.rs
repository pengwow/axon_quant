//! DeFi 核心类型

use serde::{Deserialize, Serialize};

/// EVM 链配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvmConfig {
    /// 链 ID
    pub chain_id: u64,
    /// RPC 端点
    pub rpc_url: String,
    /// 私钥（用于签名）
    pub private_key: String,
    /// 1inch API Key（可选）
    pub oneinch_api_key: Option<String>,
    /// Flashbots RPC（可选）
    pub flashbots_rpc: Option<String>,
}

impl EvmConfig {
    /// 创建新的 EVM 配置
    pub fn new(chain_id: u64, rpc_url: String, private_key: String) -> Self {
        Self {
            chain_id,
            rpc_url,
            private_key,
            oneinch_api_key: None,
            flashbots_rpc: None,
        }
    }

    /// 设置 1inch API Key
    pub fn with_oneinch_api_key(mut self, key: String) -> Self {
        self.oneinch_api_key = Some(key);
        self
    }

    /// 设置 Flashbots RPC
    pub fn with_flashbots_rpc(mut self, rpc: String) -> Self {
        self.flashbots_rpc = Some(rpc);
        self
    }

    /// 验证配置
    pub fn validate(&self) -> Result<(), crate::error::DefiError> {
        if self.rpc_url.is_empty() {
            return Err(crate::error::DefiError::ConfigError(
                "RPC URL is empty".into(),
            ));
        }
        if self.private_key.is_empty() {
            return Err(crate::error::DefiError::ConfigError(
                "Private key is empty".into(),
            ));
        }
        Ok(())
    }
}

/// DeFi 订单
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefiOrder {
    /// 代币地址
    pub token: String,
    /// 金额
    pub amount: String,
    /// 金额（USD）
    pub amount_usd: f64,
    /// 滑点（百分比）
    pub slippage: f64,
    /// 目标地址
    pub to: String,
}

impl Default for DefiOrder {
    fn default() -> Self {
        Self {
            token: String::new(),
            amount: String::new(),
            amount_usd: 0.0,
            slippage: 0.5,
            to: String::new(),
        }
    }
}

impl DefiOrder {
    /// 创建新的 DeFi 订单
    pub fn new(token: String, amount: String, amount_usd: f64) -> Self {
        Self {
            token,
            amount,
            amount_usd,
            slippage: 0.5,
            to: String::new(),
        }
    }

    /// 设置滑点
    pub fn with_slippage(mut self, slippage: f64) -> Self {
        self.slippage = slippage.clamp(0.0, 100.0);
        self
    }

    /// 设置目标地址
    pub fn with_to(mut self, to: String) -> Self {
        self.to = to;
        self
    }

    /// 验证订单
    pub fn validate(&self) -> Result<(), crate::error::DefiError> {
        if self.token.is_empty() {
            return Err(crate::error::DefiError::ConfigError(
                "Token is empty".into(),
            ));
        }
        if self.amount.is_empty() {
            return Err(crate::error::DefiError::ConfigError(
                "Amount is empty".into(),
            ));
        }
        if self.amount_usd <= 0.0 {
            return Err(crate::error::DefiError::ConfigError(
                "Amount USD must be positive".into(),
            ));
        }
        Ok(())
    }
}

/// 交易路由
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapRoute {
    /// 输入代币
    pub token_in: String,
    /// 输出代币
    pub token_out: String,
    /// 费率
    pub fee: u32,
    /// 输入金额
    pub amount_in: String,
    /// 输出金额
    pub amount_out: String,
}

/// 风控检查结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskCheckResult {
    /// 是否批准
    pub approved: bool,
    /// 原因
    pub reason: Option<String>,
    /// Gas 估算
    pub gas_estimate: Option<String>,
}

/// 跨链状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BridgeStatus {
    /// 待处理
    Pending,
    /// 源链已确认
    SrcConfirmed,
    /// 目标链已确认
    DstConfirmed,
    /// 已完成
    Completed,
    /// 失败
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evm_config_creation() {
        let config = EvmConfig::new(
            1,
            "https://mainnet.infura.io/v3/xxx".into(),
            "0x1234567890abcdef".into(),
        );
        assert_eq!(config.chain_id, 1);
        assert_eq!(config.rpc_url, "https://mainnet.infura.io/v3/xxx");
        assert!(config.oneinch_api_key.is_none());
        assert!(config.flashbots_rpc.is_none());
    }

    #[test]
    fn test_evm_config_with_oneinch() {
        let config =
            EvmConfig::new(1, "rpc".into(), "key".into()).with_oneinch_api_key("api_key".into());
        assert_eq!(config.oneinch_api_key, Some("api_key".into()));
    }

    #[test]
    fn test_evm_config_with_flashbots() {
        let config = EvmConfig::new(1, "rpc".into(), "key".into())
            .with_flashbots_rpc("https://flashbots.rpc".into());
        assert_eq!(config.flashbots_rpc, Some("https://flashbots.rpc".into()));
    }

    #[test]
    fn test_evm_config_validate_ok() {
        let config = EvmConfig::new(1, "rpc".into(), "key".into());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_evm_config_validate_empty_rpc() {
        let config = EvmConfig::new(1, "".into(), "key".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_evm_config_validate_empty_key() {
        let config = EvmConfig::new(1, "rpc".into(), "".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_evm_config_serialization() {
        let config = EvmConfig::new(1, "rpc".into(), "key".into());
        let json = serde_json::to_string(&config).unwrap();
        let restored: EvmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.chain_id, restored.chain_id);
    }

    #[test]
    fn test_defi_order_creation() {
        let order = DefiOrder::new("0xtoken".into(), "1000".into(), 50000.0);
        assert_eq!(order.token, "0xtoken");
        assert_eq!(order.amount, "1000");
        assert_eq!(order.amount_usd, 50000.0);
        assert_eq!(order.slippage, 0.5);
    }

    #[test]
    fn test_defi_order_with_slippage() {
        let order = DefiOrder::new("0xtoken".into(), "1000".into(), 50000.0).with_slippage(1.0);
        assert_eq!(order.slippage, 1.0);
    }

    #[test]
    fn test_defi_order_with_slippage_clamp() {
        let order = DefiOrder::new("0xtoken".into(), "1000".into(), 50000.0).with_slippage(150.0);
        assert_eq!(order.slippage, 100.0);
    }

    #[test]
    fn test_defi_order_with_to() {
        let order =
            DefiOrder::new("0xtoken".into(), "1000".into(), 50000.0).with_to("0xreceiver".into());
        assert_eq!(order.to, "0xreceiver");
    }

    #[test]
    fn test_defi_order_validate_ok() {
        let order = DefiOrder::new("0xtoken".into(), "1000".into(), 50000.0);
        assert!(order.validate().is_ok());
    }

    #[test]
    fn test_defi_order_validate_empty_token() {
        let order = DefiOrder::new("".into(), "1000".into(), 50000.0);
        assert!(order.validate().is_err());
    }

    #[test]
    fn test_defi_order_validate_empty_amount() {
        let order = DefiOrder::new("0xtoken".into(), "".into(), 50000.0);
        assert!(order.validate().is_err());
    }

    #[test]
    fn test_defi_order_validate_negative_usd() {
        let order = DefiOrder::new("0xtoken".into(), "1000".into(), -1.0);
        assert!(order.validate().is_err());
    }

    #[test]
    fn test_defi_order_serialization() {
        let order = DefiOrder::new("0xtoken".into(), "1000".into(), 50000.0);
        let json = serde_json::to_string(&order).unwrap();
        let restored: DefiOrder = serde_json::from_str(&json).unwrap();
        assert_eq!(order.token, restored.token);
    }

    #[test]
    fn test_swap_route_serialization() {
        let route = SwapRoute {
            token_in: "0xA".into(),
            token_out: "0xB".into(),
            fee: 3000,
            amount_in: "1000".into(),
            amount_out: "999".into(),
        };
        let json = serde_json::to_string(&route).unwrap();
        let restored: SwapRoute = serde_json::from_str(&json).unwrap();
        assert_eq!(route.fee, restored.fee);
    }

    #[test]
    fn test_risk_check_result_approved() {
        let result = RiskCheckResult {
            approved: true,
            reason: None,
            gas_estimate: Some("21000".into()),
        };
        assert!(result.approved);
        assert!(result.reason.is_none());
    }

    #[test]
    fn test_bridge_status_variants() {
        let statuses = [
            BridgeStatus::Pending,
            BridgeStatus::SrcConfirmed,
            BridgeStatus::DstConfirmed,
            BridgeStatus::Completed,
            BridgeStatus::Failed("error".into()),
        ];
        assert_eq!(statuses.len(), 5);
    }
}
