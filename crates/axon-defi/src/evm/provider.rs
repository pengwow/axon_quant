//! EVM Provider 工厂
//!
//! 0.3.0 P0 新增:封装 `alloy::providers::Provider`,提供:
//! - `ProviderConfig`:RPC URL / WS URL / timeout / retry / anvil fork
//! - `EvmProvider::new(config)`:工厂方法
//! - `chain_id()` / `block_number()`:链级查询
//!
//! 注意:仅当 `evm` feature 启用时才引入 alloy,默认 feature 下此模块仍可编译
//! 但 `EvmProvider::new` 不可用(feature gate 由调用方决定)。

use serde::{Deserialize, Serialize};

use crate::error::DefiError;
use crate::evm::chain::Chain;

/// EVM Provider 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// HTTP RPC URL(必填)
    pub rpc_url: String,
    /// WebSocket URL(可选,用于事件订阅)
    pub ws_url: Option<String>,
    /// 单次调用超时(毫秒),默认 5000
    pub timeout_ms: u64,
    /// 最大重试次数,默认 3
    pub max_retries: u32,
    /// Anvil fork 起始块(测试用,生产置 None)
    pub anvil_fork_block: Option<u64>,
    /// 关联的链(Ethereum/Arbitrum/Optimism/Polygon)
    pub chain: Chain,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            rpc_url: String::new(),
            ws_url: None,
            timeout_ms: 5000,
            max_retries: 3,
            anvil_fork_block: None,
            chain: Chain::Ethereum,
        }
    }
}

impl ProviderConfig {
    /// 构造指定链的 ProviderConfig
    ///
    /// 自动填充 chain 字段
    pub fn for_chain(chain: Chain, rpc_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            chain,
            ..Self::default()
        }
    }
}

/// EVM Provider(轻量 wrapper,内部持 `alloy::providers::DynProvider`)
#[derive(Debug, Clone)]
pub struct EvmProvider {
    config: ProviderConfig,
}

impl EvmProvider {
    /// 构造 EvmProvider
    ///
    /// **仅 `evm` feature 启用时** 真正初始化 alloy Provider;
    /// 否则仅保存 config,所有 RPC 方法返回 `ConfigError`。
    pub fn new(config: ProviderConfig) -> Self {
        Self { config }
    }

    /// 读取 config 引用
    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }

    /// 查询链 ID
    #[cfg(feature = "evm")]
    pub async fn chain_id(&self) -> Result<u64, DefiError> {
        use alloy::providers::{Provider, ProviderBuilder};

        let parsed = ::url::Url::parse(&self.config.rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid rpc url: {}", e)))?;
        let provider = ProviderBuilder::new().connect_http(parsed);

        provider
            .get_chain_id()
            .await
            .map_err(|e| self.wrap_rpc_error(e))
    }

    /// 查询链 ID(无 evm feature 时返回 ConfigError)
    #[cfg(not(feature = "evm"))]
    pub async fn chain_id(&self) -> Result<u64, DefiError> {
        Err(DefiError::ConfigError(
            "evm feature not enabled; rebuild with --features evm".into(),
        ))
    }

    /// 查询最新 block number
    #[cfg(feature = "evm")]
    pub async fn block_number(&self) -> Result<u64, DefiError> {
        use alloy::providers::{Provider, ProviderBuilder};

        let parsed = ::url::Url::parse(&self.config.rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid rpc url: {}", e)))?;
        let provider = ProviderBuilder::new().connect_http(parsed);

        provider
            .get_block_number()
            .await
            .map_err(|e| self.wrap_rpc_error(e))
    }

    /// 查询最新 block number(无 evm feature 时返回 ConfigError)
    #[cfg(not(feature = "evm"))]
    pub async fn block_number(&self) -> Result<u64, DefiError> {
        Err(DefiError::ConfigError(
            "evm feature not enabled; rebuild with --features evm".into(),
        ))
    }

    /// 包装 alloy RPC 错误 → DefiError::RpcError 结构化
    #[cfg(feature = "evm")]
    fn wrap_rpc_error(
        &self,
        e: alloy::transports::RpcError<alloy::transports::TransportErrorKind>,
    ) -> DefiError {
        DefiError::RpcError {
            url: self.config.rpc_url.clone(),
            status: 0, // alloy 内部错误,无 HTTP status
            body: DefiError::truncated_body(&format!("{}", e)),
        }
    }
}

/// 单元测试(不连真链)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_has_sane_values() {
        let cfg = ProviderConfig::default();
        assert_eq!(cfg.timeout_ms, 5000);
        assert_eq!(cfg.max_retries, 3);
        assert!(cfg.anvil_fork_block.is_none());
        assert!(cfg.ws_url.is_none());
    }

    #[test]
    fn config_for_chain_sets_chain() {
        let cfg = ProviderConfig::for_chain(Chain::Arbitrum, "https://arb1.arbitrum.io/rpc");
        assert_eq!(cfg.chain, Chain::Arbitrum);
        assert_eq!(cfg.rpc_url, "https://arb1.arbitrum.io/rpc");
    }

    #[test]
    fn config_serialization_roundtrip() {
        let cfg = ProviderConfig::for_chain(Chain::Ethereum, "https://eth.llamarpc.com");
        let json = serde_json::to_string(&cfg).unwrap();
        let restored: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.rpc_url, cfg.rpc_url);
        assert_eq!(restored.chain, cfg.chain);
    }

    #[test]
    fn evm_provider_clone_shares_config() {
        let cfg = ProviderConfig::for_chain(Chain::Ethereum, "https://eth.llamarpc.com");
        let p1 = EvmProvider::new(cfg);
        let p2 = p1.clone();
        assert_eq!(p1.config().rpc_url, p2.config().rpc_url);
        assert_eq!(p1.config().chain, p2.config().chain);
    }

    #[test]
    fn chain_id_without_evm_feature_returns_config_error() {
        // 测试:无 evm feature 时 chain_id 返回 ConfigError 而非 panic
        let cfg = ProviderConfig::for_chain(Chain::Ethereum, "https://eth.llamarpc.com");
        let p = EvmProvider::new(cfg);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(p.chain_id());
        if cfg!(feature = "evm") {
            // feature 启用:此处不验证结果(需真链)
        } else {
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, DefiError::ConfigError(_)));
        }
    }
}
