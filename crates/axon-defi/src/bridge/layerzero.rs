//! LayerZero V2 跨链桥(0.3.0 P0 Batch 4 / T1.11)
//!
//! 0.3.0 改造:`bridge_tokens` 不再返回 `format!("0x{:064x}", 67890)` 假 hash,
//! 改走 [LayerZeroV2Endpoint] `quote()` + `send()` 真链交互。
//!
//! 关键设计:
//! - LayerZero V2 EndpointV2 4 链共用同一地址:`0x1a44076050125825900e736c501f859c50fE728c`
//! - `quote(MessagingParams, payInLzToken)` 拿 native fee
//! - `send(MessagingParams, refund)` 实际发送(带 value = native fee)
//! - 调用方需先 approve token 给 OFT/OApp adapter(本模块不内联,留给上层)

use serde::{Deserialize, Serialize};

use crate::error::DefiError;
use crate::evm::chain::Chain;
use crate::evm::provider::EvmProvider;
use crate::evm::signer::LocalSigner;

#[cfg(feature = "evm")]
use alloy::network::{EthereumWallet, TransactionBuilder};
#[cfg(feature = "evm")]
use alloy::primitives::{Address, U256};
#[cfg(feature = "evm")]
use alloy::providers::Provider;
#[cfg(feature = "evm")]
use alloy::rpc::types::TransactionReceipt;
#[cfg(feature = "evm")]
use alloy::sol;

/// LayerZero V2 EndpointV2 通用地址
///
/// mainnet / Arbitrum / Optimism / Polygon / Base 等同一地址(Canonical by LayerZero Labs)
pub const LZ_ENDPOINT_V2_ADDRESS: &str = "0x1a44076050125825900e736c501f859c50fE728c";

#[cfg(feature = "evm")]
sol! {
    #[sol(rpc)]
    interface ILayerZeroEndpointV2 {
        struct MessagingParams {
            uint32 dstEid;
            bytes32 receiver;
            bytes message;
            bytes options;
            bool payInLzToken;
        }
        struct MessagingFee {
            uint256 nativeFee;
            uint256 lzTokenFee;
        }
        struct MessagingReceipt {
            bytes32 guid;
            uint64 nonce;
            bytes payload;
        }
        function quote(MessagingParams calldata params, bool payInLzToken)
            external view returns (MessagingFee memory fee);
        function send(MessagingParams calldata params, bool refund)
            external payable returns (MessagingReceipt memory receipt);
    }
}

/// 跨链桥配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    /// LayerZero V2 端点合约地址
    pub endpoint: String,
    /// 支持的链(chain_id)
    pub supported_chains: Vec<u64>,
    /// 默认滑点(预留,实际 fee 走 quote)
    pub default_slippage: f64,
    /// 超时时间(秒,留给上层轮询用)
    pub timeout_secs: u64,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            endpoint: LZ_ENDPOINT_V2_ADDRESS.to_string(),
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

/// 跨链消息参数
#[cfg(feature = "evm")]
#[derive(Debug, Clone)]
pub struct MessagingParamsInput {
    /// 目标链 LayerZero EID
    pub dst_eid: u32,
    /// 接收者(bytes32,右 padded address)
    pub receiver_bytes32: [u8; 32],
    /// 编码后的 message(本模块不内联,OApp/OFT 自定义)
    pub message: Vec<u8>,
    /// LayerZero options(executor lzReceive option,默认空)
    pub options: Vec<u8>,
    /// 用 LZ token 付 fee(false = native)
    pub pay_in_lz_token: bool,
}

/// 跨链桥管理器
#[derive(Debug, Clone)]
pub struct BridgeManager {
    config: BridgeConfig,
}

impl BridgeManager {
    /// 创建新的管理器
    pub fn new(config: BridgeConfig) -> Self {
        Self { config }
    }

    /// 从默认配置创建
    pub fn default_for_chain(_chain: &Chain) -> Self {
        Self::new(BridgeConfig::default())
    }

    /// 获取配置
    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    /// 验证目标链支持
    pub fn is_supported(&self, dst_chain: &Chain) -> bool {
        self.config.supported_chains.contains(&dst_chain.chain_id())
    }

    /// 估算 native fee(走真 EndpointV2.quote)
    #[cfg(feature = "evm")]
    pub async fn estimate_fee(
        &self,
        provider: &EvmProvider,
        params: &MessagingParamsInput,
    ) -> Result<U256, DefiError> {
        use alloy::sol_types::SolValue;
        let endpoint: Address = self
            .config
            .endpoint
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid endpoint: {}", e)))?;
        let parsed_url = ::url::Url::parse(&provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new().connect_http(parsed_url);

        let lz_params = ILayerZeroEndpointV2::MessagingParams {
            dstEid: params.dst_eid,
            receiver: alloy::primitives::B256::from(params.receiver_bytes32),
            message: params.message.clone().into(),
            options: params.options.clone().into(),
            payInLzToken: params.pay_in_lz_token,
        };
        // quote(calldata) is `quote(MessagingParams, bool)`,but as calldata 编码时 struct 嵌套需要手工
        // 这里简化:走 sendTransaction 的 eth_call 模拟获取回执
        // 直接构造 quote selector + 参数 = 0x7d2cb0f2 + abi.encode(MessagingParams, bool)
        use alloy::sol_types::SolCall;
        let call = ILayerZeroEndpointV2::quoteCall {
            params: lz_params,
            payInLzToken: params.pay_in_lz_token,
        };
        let input = call.abi_encode();
        let tx = alloy::rpc::types::TransactionRequest::default()
            .with_to(endpoint)
            .with_input(input);
        let output: alloy::primitives::Bytes =
            p.call(tx).await.map_err(|e| DefiError::RpcError {
                url: provider.config().rpc_url.clone(),
                status: 0,
                body: DefiError::truncated_body(&format!("{}", e)),
            })?;
        // 返回值是 (uint256 nativeFee, uint256 lzTokenFee)
        type FeeReturn = (U256, U256);
        let fee: FeeReturn =
            SolValue::abi_decode(&output).map_err(|e| DefiError::ContractError {
                address: self.config.endpoint.clone(),
                method: "quote".into(),
                reason: format!("decode: {}", e),
            })?;
        Ok(fee.0) // nativeFee
    }

    /// estimate_fee stub
    #[cfg(not(feature = "evm"))]
    pub async fn estimate_fee(
        &self,
        _provider: &EvmProvider,
        _params: &crate::bridge::MessagingParamsInput,
    ) -> Result<String, DefiError> {
        Err(DefiError::ConfigError("evm feature not enabled".into()))
    }

    /// 发起跨链转账(走真 EndpointV2.send)
    ///
    /// 0.3.0 改造:不再返回 `format!("0x{:064x}", 67890)` 假 hash,
    /// 改走 `send(MessagingParams, refund)` 实际发交易,带 native fee。
    #[cfg(feature = "evm")]
    pub async fn bridge_tokens(
        &self,
        signer: &LocalSigner,
        provider: &EvmProvider,
        dst_chain: &Chain,
        params: &MessagingParamsInput,
    ) -> Result<TransactionReceipt, DefiError> {
        // 校验目标链
        if !self.is_supported(dst_chain) {
            return Err(DefiError::UnsupportedChain(dst_chain.chain_id()));
        }

        // 先 quote 拿 native fee
        let native_fee = self.estimate_fee(provider, params).await?;

        let endpoint: Address = self
            .config
            .endpoint
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid endpoint: {}", e)))?;
        let parsed_url = ::url::Url::parse(&provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new()
            .wallet(EthereumWallet::from(signer.raw_signer().clone()))
            .connect_http(parsed_url);

        let lz_params = ILayerZeroEndpointV2::MessagingParams {
            dstEid: params.dst_eid,
            receiver: alloy::primitives::B256::from(params.receiver_bytes32),
            message: params.message.clone().into(),
            options: params.options.clone().into(),
            payInLzToken: params.pay_in_lz_token,
        };
        use alloy::sol_types::SolCall;
        let call = ILayerZeroEndpointV2::sendCall {
            params: lz_params,
            refund: false, // 不退费,直接用收款人做 refund
        };
        let data = call.abi_encode();
        let nonce = signer.next_nonce();
        let tx = alloy::rpc::types::TransactionRequest::default()
            .with_to(endpoint)
            .with_input(data)
            .with_nonce(nonce)
            .with_value(native_fee); // 必带 native fee

        let pending = p
            .send_transaction(tx)
            .await
            .map_err(|e| DefiError::RpcError {
                url: provider.config().rpc_url.clone(),
                status: 0,
                body: DefiError::truncated_body(&format!("{}", e)),
            })?;
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|e| DefiError::RpcError {
                url: provider.config().rpc_url.clone(),
                status: 0,
                body: DefiError::truncated_body(&format!("{}", e)),
            })?;
        Ok(receipt)
    }

    /// bridge_tokens stub
    #[cfg(not(feature = "evm"))]
    pub async fn bridge_tokens(
        &self,
        _signer: &LocalSigner,
        _provider: &EvmProvider,
        _dst_chain: &Chain,
        _params: &crate::bridge::MessagingParamsInput,
    ) -> Result<String, DefiError> {
        Err(DefiError::ConfigError("evm feature not enabled".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_lz_endpoint_address() {
        assert_eq!(
            LZ_ENDPOINT_V2_ADDRESS.to_lowercase(),
            "0x1a44076050125825900e736c501f859c50fe728c"
        );
    }

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

    #[test]
    fn test_bridge_manager_is_supported() {
        let manager = BridgeManager::new(BridgeConfig::default());
        assert!(manager.is_supported(&Chain::Ethereum));
        assert!(manager.is_supported(&Chain::Arbitrum));
        assert!(manager.is_supported(&Chain::Optimism));
        assert!(manager.is_supported(&Chain::Polygon));
    }

    #[test]
    fn test_bridge_manager_unsupported_chain() {
        // 改用空 chains 列表验证
        let config = BridgeConfig {
            supported_chains: vec![],
            ..Default::default()
        };
        let manager = BridgeManager::new(config);
        assert!(!manager.is_supported(&Chain::Ethereum));
    }
}
