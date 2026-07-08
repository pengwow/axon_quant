//! EVM ERC-20 客户端
//!
//! 0.3.0 P0:封装 IERC20 合约,提供:
//! - `TokenInfo`:地址 + decimals + symbol(可选缓存)
//! - `Erc20Client::new(addr, provider)` 工厂
//! - 读路径:`decimals()` / `symbol()` / `balance_of()`
//! - 已知 token 预设(USDC/USDT/DAI/WETH)避免重复 RPC
//!
//! 写路径(`approve` / `transfer`)在 `evm_erc20_write.rs` 测试覆盖,
//! 见 [evm_erc20.rs](../tests/evm_erc20.rs) 的姊妹测试。

use serde::{Deserialize, Serialize};

use crate::error::DefiError;
use crate::evm::provider::EvmProvider;

#[cfg(feature = "evm")]
use alloy::network::TransactionBuilder;
#[cfg(feature = "evm")]
use alloy::primitives::{Address, U256};
#[cfg(feature = "evm")]
use alloy::providers::Provider;
#[cfg(feature = "evm")]
use alloy::rpc::types::TransactionRequest;
#[cfg(feature = "evm")]
use alloy::sol;
#[cfg(feature = "evm")]
use alloy::sol_types::{SolCall, SolEvent, SolValue};

#[cfg(feature = "evm")]
sol! {
    #[derive(Debug)]
    interface IERC20 {
        function decimals() external view returns (uint8);
        function symbol() external view returns (string);
        function balanceOf(address account) external view returns (uint256);
        function approve(address spender, uint256 amount) external returns (bool);
        function transfer(address to, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);

        event Transfer(address indexed from, address indexed to, uint256 value);
        event Approval(address indexed owner, address indexed spender, uint256 value);
    }
}

/// Token 元信息
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    /// 合约地址(始终 lowercase)
    pub address: String,
    /// decimals(可选缓存)
    pub decimals: Option<u8>,
    /// symbol(可选缓存)
    pub symbol: Option<String>,
}

impl TokenInfo {
    /// 新建(地址自动 lowercase,其余 None)
    pub fn new(address: &str) -> Self {
        Self {
            address: address.to_lowercase(),
            decimals: None,
            symbol: None,
        }
    }

    /// 新建并按已知 token 预设(USDC/USDT/DAI/WETH)
    pub fn with_known_token(address: &str) -> Self {
        let mut info = Self::new(address);
        match info.address.as_str() {
            // Ethereum mainnet 已知 token
            "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48" => {
                info.decimals = Some(6);
                info.symbol = Some("USDC".to_string());
            }
            "0xdac17f958d2ee523a2206206994597c13d831ec7" => {
                info.decimals = Some(6);
                info.symbol = Some("USDT".to_string());
            }
            "0x6b175474e89094c44da98b954eedeac495271d0f" => {
                info.decimals = Some(18);
                info.symbol = Some("DAI".to_string());
            }
            "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2" => {
                info.decimals = Some(18);
                info.symbol = Some("WETH".to_string());
            }
            _ => {}
        }
        info
    }
}

/// ERC-20 客户端
#[derive(Debug, Clone)]
pub struct Erc20Client {
    info: TokenInfo,
    provider: EvmProvider,
}

impl Erc20Client {
    /// 构造 ERC-20 客户端(使用 `TokenInfo::with_known_token` 预设常见 token)
    pub fn new(address: &str, provider: EvmProvider) -> Self {
        Self {
            info: TokenInfo::with_known_token(address),
            provider,
        }
    }

    /// 读取 token 元信息
    pub fn info(&self) -> &TokenInfo {
        &self.info
    }

    /// 查询 decimals(从链上;若已知则直接返回)
    #[cfg(feature = "evm")]
    pub async fn decimals(&self) -> Result<u8, DefiError> {
        if let Some(d) = self.info.decimals {
            return Ok(d);
        }
        let addr: Address = self
            .info
            .address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid token address: {}", e)))?;
        let parsed_url = ::url::Url::parse(&self.provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new().connect_http(parsed_url);

        // 用 TransactionRequest::call + ABI 解码
        let call = IERC20::decimalsCall {};
        let input = call.abi_encode();
        let tx = TransactionRequest::default()
            .with_to(addr)
            .with_input(input);
        let output: alloy::primitives::Bytes =
            p.call(tx).await.map_err(|e| self.wrap_rpc_error(e))?;
        // ABI 编码 uint8:32 字节 padded,值在最后一字节
        if output.len() < 32 {
            return Err(DefiError::ContractError {
                address: self.info.address.clone(),
                method: "decimals".into(),
                reason: format!("output too short: {} bytes", output.len()),
            });
        }
        Ok(output[31])
    }

    /// decimals stub
    #[cfg(not(feature = "evm"))]
    pub async fn decimals(&self) -> Result<u8, DefiError> {
        Err(DefiError::ConfigError("evm feature not enabled".into()))
    }

    /// 查询 symbol(从链上;若已知则直接返回)
    #[cfg(feature = "evm")]
    pub async fn symbol(&self) -> Result<String, DefiError> {
        if let Some(ref s) = self.info.symbol {
            return Ok(s.clone());
        }
        let addr: Address = self
            .info
            .address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid token address: {}", e)))?;
        let parsed_url = ::url::Url::parse(&self.provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new().connect_http(parsed_url);

        let call = IERC20::symbolCall {};
        let input = call.abi_encode();
        let tx = TransactionRequest::default()
            .with_to(addr)
            .with_input(input);
        let output: alloy::primitives::Bytes =
            p.call(tx).await.map_err(|e| self.wrap_rpc_error(e))?;
        // alloy 1.6:String::abi_decode 接受单参(默认 validate)
        let result = String::abi_decode(&output).map_err(|e| DefiError::ContractError {
            address: self.info.address.clone(),
            method: "symbol".into(),
            reason: format!("decode: {}", e),
        })?;
        Ok(result)
    }

    /// symbol stub
    #[cfg(not(feature = "evm"))]
    pub async fn symbol(&self) -> Result<String, DefiError> {
        Err(DefiError::ConfigError("evm feature not enabled".into()))
    }

    /// 查询 ERC-20 余额
    #[cfg(feature = "evm")]
    pub async fn balance_of(&self, holder: &str) -> Result<U256, DefiError> {
        let token: Address = self
            .info
            .address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid token: {}", e)))?;
        let holder: Address = holder
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid holder: {}", e)))?;
        let parsed_url = ::url::Url::parse(&self.provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new().connect_http(parsed_url);

        let call = IERC20::balanceOfCall { account: holder };
        let input = call.abi_encode();
        let tx = TransactionRequest::default()
            .with_to(token)
            .with_input(input);
        let output: alloy::primitives::Bytes =
            p.call(tx).await.map_err(|e| self.wrap_rpc_error(e))?;
        // alloy 1.6:U256::abi_decode 接受单参
        let result = U256::abi_decode(&output).map_err(|e| DefiError::ContractError {
            address: self.info.address.clone(),
            method: "balanceOf".into(),
            reason: format!("decode: {}", e),
        })?;
        Ok(result)
    }

    /// balance_of stub
    #[cfg(not(feature = "evm"))]
    pub async fn balance_of(&self, _holder: &str) -> Result<String, DefiError> {
        Err(DefiError::ConfigError("evm feature not enabled".into()))
    }

    /// 写路径 - approve(需要 signer,见 evm/erc20_write)
    #[cfg(feature = "evm")]
    pub fn approve_tx(
        &self,
        spender: Address,
        amount: U256,
    ) -> Result<TransactionRequest, DefiError> {
        let token: Address = self
            .info
            .address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid token: {}", e)))?;
        let call = IERC20::approveCall { spender, amount };
        let data = call.abi_encode();
        Ok(TransactionRequest::default()
            .with_to(token)
            .with_input(data))
    }

    /// 写路径 - transfer(需要 signer)
    #[cfg(feature = "evm")]
    pub fn transfer_tx(&self, to: Address, amount: U256) -> Result<TransactionRequest, DefiError> {
        let token: Address = self
            .info
            .address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid token: {}", e)))?;
        let call = IERC20::transferCall { to, amount };
        let data = call.abi_encode();
        Ok(TransactionRequest::default()
            .with_to(token)
            .with_input(data))
    }

    /// 写路径 - 完整发送 approve(签名 + send + 等待 receipt)
    ///
    /// 需 `LocalSigner` 提供签名 + nonce
    #[cfg(feature = "evm")]
    pub async fn approve(
        &self,
        signer: &crate::evm::signer::LocalSigner,
        provider: &EvmProvider,
        spender: Address,
        amount: U256,
    ) -> Result<alloy::rpc::types::TransactionReceipt, DefiError> {
        use alloy::network::{EthereumWallet, TransactionBuilder};
        use alloy::providers::Provider;

        let token: Address = self
            .info
            .address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid token: {}", e)))?;
        let parsed_url = ::url::Url::parse(&provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new()
            .wallet(EthereumWallet::from(signer.raw_signer().clone()))
            .connect_http(parsed_url);

        let call = IERC20::approveCall { spender, amount };
        let data = call.abi_encode();
        let nonce = signer.next_nonce();
        let tx = TransactionRequest::default()
            .with_to(token)
            .with_input(data)
            .with_nonce(nonce);

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

    /// 写路径 - 完整发送 transfer
    #[cfg(feature = "evm")]
    pub async fn transfer(
        &self,
        signer: &crate::evm::signer::LocalSigner,
        provider: &EvmProvider,
        to: Address,
        amount: U256,
    ) -> Result<alloy::rpc::types::TransactionReceipt, DefiError> {
        use alloy::network::{EthereumWallet, TransactionBuilder};
        use alloy::providers::Provider;

        let token: Address = self
            .info
            .address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid token: {}", e)))?;
        let parsed_url = ::url::Url::parse(&provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new()
            .wallet(EthereumWallet::from(signer.raw_signer().clone()))
            .connect_http(parsed_url);

        let call = IERC20::transferCall { to, amount };
        let data = call.abi_encode();
        let nonce = signer.next_nonce();
        let tx = TransactionRequest::default()
            .with_to(token)
            .with_input(data)
            .with_nonce(nonce);

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

    /// 写路径 - 解析 Transfer 事件日志
    ///
    /// `topics` 必须包含事件 signature + indexed 参数 (from, to)
    /// `data` 是非 indexed 数据 (value)
    #[cfg(feature = "evm")]
    pub fn parse_transfer_log(
        topics: &[alloy::primitives::B256],
        data: &[u8],
    ) -> Result<(Address, Address, U256), DefiError> {
        let log = IERC20::Transfer::decode_raw_log(topics.iter().copied(), data).map_err(|e| {
            DefiError::ContractError {
                address: "ERC20".into(),
                method: "Transfer".into(),
                reason: format!("log decode: {}", e),
            }
        })?;
        Ok((log.from, log.to, log.value))
    }

    /// 写路径 - 解析 Approval 事件日志
    #[cfg(feature = "evm")]
    pub fn parse_approval_log(
        topics: &[alloy::primitives::B256],
        data: &[u8],
    ) -> Result<(Address, Address, U256), DefiError> {
        let log = IERC20::Approval::decode_raw_log(topics.iter().copied(), data).map_err(|e| {
            DefiError::ContractError {
                address: "ERC20".into(),
                method: "Approval".into(),
                reason: format!("log decode: {}", e),
            }
        })?;
        Ok((log.owner, log.spender, log.value))
    }

    /// 包装 alloy RPC 错误
    #[cfg(feature = "evm")]
    fn wrap_rpc_error(
        &self,
        e: alloy::transports::RpcError<alloy::transports::TransportErrorKind>,
    ) -> DefiError {
        DefiError::RpcError {
            url: self.provider.config().rpc_url.clone(),
            status: 0,
            body: DefiError::truncated_body(&format!("{}", e)),
        }
    }
}

/// 单元测试(不连真链)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_info_lowercases_address() {
        let info = TokenInfo::new("0xA0B86991C6218B36C1D19D4A2E9EB0CE3606EB48");
        assert_eq!(info.address, "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
    }

    #[test]
    fn known_usdc_has_decimals_6() {
        let info = TokenInfo::with_known_token("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        assert_eq!(info.decimals, Some(6));
        assert_eq!(info.symbol.as_deref(), Some("USDC"));
    }

    #[test]
    fn known_weth_has_decimals_18() {
        let info = TokenInfo::with_known_token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        assert_eq!(info.decimals, Some(18));
        assert_eq!(info.symbol.as_deref(), Some("WETH"));
    }

    #[test]
    fn unknown_token_returns_no_preset() {
        let info = TokenInfo::with_known_token("0x1234567890123456789012345678901234567890");
        assert!(info.decimals.is_none());
        assert!(info.symbol.is_none());
    }

    #[test]
    fn token_info_serializes_roundtrip() {
        let info = TokenInfo::with_known_token("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let json = serde_json::to_string(&info).unwrap();
        let restored: TokenInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, restored);
    }
}
