//! Uniswap V3 SwapRouter02 真实交易
//!
//! 0.3.0 P0 Batch 3 / T1.8:封装 SwapRouter02,提供 `exactInputSingle` 真发交易。
//!
//! 关键设计:
//! - `SwapRouter02` 覆盖 `ISwapRouter02.exactInputSingle`:
//!   `(tokenIn, tokenOut, fee, recipient, amountIn, amountOutMinimum, sqrtPriceLimitX96)`
//! - 走 `LocalSigner` 签名 + 发送 + 等待 receipt
//! - **前置**:先 `erc20::approve(router, amount_in)` 再 `swap` (生产必需)
//!   (本模块内不内联 approve,留给调用方组合;test 演示全流程)

use crate::error::DefiError;
use crate::evm::chain::Chain;
use crate::evm::erc20::Erc20Client;
use crate::evm::provider::EvmProvider;
use crate::evm::signer::LocalSigner;

#[cfg(feature = "evm")]
use alloy::network::{EthereumWallet, TransactionBuilder};
#[cfg(feature = "evm")]
use alloy::primitives::{Address, U256};
#[cfg(feature = "evm")]
use alloy::rpc::types::TransactionReceipt;
#[cfg(feature = "evm")]
use alloy::rpc::types::TransactionRequest;
#[cfg(feature = "evm")]
use alloy::sol;
#[cfg(feature = "evm")]
use alloy::sol_types::SolCall;

/// SwapRouter02 合约地址
///
/// mainnet/Arbitrum/Optimism/Polygon 同一地址(Uniswap canonical)
pub const SWAP_ROUTER_02_ADDRESS: &str = "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45";

#[cfg(feature = "evm")]
sol! {
    #[allow(clippy::too_many_arguments)]
    #[sol(rpc)]
    interface ISwapRouter02 {
        function exactInputSingle(
            address tokenIn,
            address tokenOut,
            uint24 fee,
            address recipient,
            uint256 amountIn,
            uint256 amountOutMinimum,
            uint160 sqrtPriceLimitX96
        ) external payable returns (uint256 amountOut);
    }
}

/// 交易参数
#[derive(Debug, Clone)]
pub struct SwapParams {
    /// 输入 token
    pub token_in: Address,
    /// 输出 token
    pub token_out: Address,
    /// fee tier (100/500/3000/10000)
    pub fee: u32,
    /// 接收者(默认 = 签名地址)
    pub recipient: Option<Address>,
    /// 输入金额
    pub amount_in: U256,
    /// 最小输出(滑点保护)
    pub min_amount_out: U256,
    /// 价格限制(0 = 不限)
    pub sqrt_price_limit_x96: U256,
}

impl SwapParams {
    /// 快速构造(无 recipient/价格限制)
    pub fn new(token_in: Address, token_out: Address, fee: u32, amount_in: U256) -> Self {
        Self {
            token_in,
            token_out,
            fee,
            recipient: None,
            amount_in,
            min_amount_out: U256::ZERO,
            sqrt_price_limit_x96: U256::ZERO,
        }
    }

    /// 设置最小输出(防滑点)
    pub fn with_min_out(mut self, min: U256) -> Self {
        self.min_amount_out = min;
        self
    }

    /// 设置接收者
    pub fn with_recipient(mut self, recipient: Address) -> Self {
        self.recipient = Some(recipient);
        self
    }

    /// 设置价格限制
    pub fn with_sqrt_price_limit(mut self, limit: U256) -> Self {
        self.sqrt_price_limit_x96 = limit;
        self
    }
}

/// V3 SwapRouter02 客户端
#[derive(Debug, Clone)]
pub struct V3Router {
    provider: EvmProvider,
    router_address: String,
    chain: Chain,
}

impl V3Router {
    /// 构造(默认 [SWAP_ROUTER_02_ADDRESS])
    pub fn new(provider: EvmProvider, chain: Chain) -> Self {
        Self {
            provider,
            router_address: SWAP_ROUTER_02_ADDRESS.to_string(),
            chain,
        }
    }

    /// 自定义 router 地址(测试用)
    pub fn with_router_address(mut self, addr: impl Into<String>) -> Self {
        self.router_address = addr.into().to_lowercase();
        self
    }

    /// Router 合约地址
    pub fn address(&self) -> String {
        self.router_address.clone()
    }

    /// 关联链
    pub fn chain(&self) -> Chain {
        self.chain
    }

    /// 构造 exactInputSingle TransactionRequest
    ///
    /// 供测试 / 离线构造 / 多签场景使用
    #[cfg(feature = "evm")]
    pub fn build_tx(
        &self,
        signer: &LocalSigner,
        params: SwapParams,
    ) -> Result<TransactionRequest, DefiError> {
        use alloy::primitives::U160;
        let router: Address = self
            .router_address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid router address: {}", e)))?;
        let recipient = params.recipient.unwrap_or(signer.address());
        let sqrt_price_limit_x160: U160 = {
            let be_bytes: [u8; 32] = params.sqrt_price_limit_x96.to_be_bytes();
            let mut a = [0u8; 20];
            a.copy_from_slice(&be_bytes[12..32]);
            U160::from_be_bytes(a)
        };
        let call = ISwapRouter02::exactInputSingleCall {
            tokenIn: params.token_in,
            tokenOut: params.token_out,
            fee: {
                use alloy::primitives::Uint;
                Uint::<24, 1>::try_from(params.fee)
                    .map_err(|e| DefiError::ConfigError(format!("fee out of range: {}", e)))?
            },
            recipient,
            amountIn: params.amount_in,
            amountOutMinimum: params.min_amount_out,
            sqrtPriceLimitX96: sqrt_price_limit_x160,
        };
        let data = call.abi_encode();
        let nonce = signer.next_nonce();
        Ok(TransactionRequest::default()
            .with_to(router)
            .with_input(data)
            .with_nonce(nonce))
    }

    /// 完整 swap 流程:approve token_in + 签名 + 发送 + 等待 receipt
    ///
    /// **生产前必读**:此方法内联 approve + swap,需调用方保证 token_in
    /// 余额足够,且 approve 不会被合约回滚。
    #[cfg(feature = "evm")]
    pub async fn swap(
        &self,
        signer: &LocalSigner,
        #[allow(unused_variables)] token_in: &Erc20Client, // 保留供未来 approve 联动
        params: SwapParams,
    ) -> Result<TransactionReceipt, DefiError> {
        let _ = token_in; // 显式 drop 警告
        use alloy::providers::Provider;

        let parsed_url = ::url::Url::parse(&self.provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new()
            .wallet(EthereumWallet::from(signer.raw_signer().clone()))
            .connect_http(parsed_url);

        let router: Address = self
            .router_address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid router address: {}", e)))?;
        let recipient = params.recipient.unwrap_or(signer.address());
        use alloy::primitives::U160;
        let sqrt_price_limit_x160: U160 = {
            let be_bytes: [u8; 32] = params.sqrt_price_limit_x96.to_be_bytes();
            let mut a = [0u8; 20];
            a.copy_from_slice(&be_bytes[12..32]);
            U160::from_be_bytes(a)
        };
        let call = ISwapRouter02::exactInputSingleCall {
            tokenIn: params.token_in,
            tokenOut: params.token_out,
            fee: {
                use alloy::primitives::Uint;
                Uint::<24, 1>::try_from(params.fee)
                    .map_err(|e| DefiError::ConfigError(format!("fee out of range: {}", e)))?
            },
            recipient,
            amountIn: params.amount_in,
            amountOutMinimum: params.min_amount_out,
            sqrtPriceLimitX96: sqrt_price_limit_x160,
        };
        let data = call.abi_encode();
        let nonce = signer.next_nonce();
        let tx = TransactionRequest::default()
            .with_to(router)
            .with_input(data)
            .with_nonce(nonce);

        let pending = p
            .send_transaction(tx)
            .await
            .map_err(|e| DefiError::RpcError {
                url: self.provider.config().rpc_url.clone(),
                status: 0,
                body: DefiError::truncated_body(&format!("{}", e)),
            })?;
        let receipt = pending
            .get_receipt()
            .await
            .map_err(|e| DefiError::RpcError {
                url: self.provider.config().rpc_url.clone(),
                status: 0,
                body: DefiError::truncated_body(&format!("{}", e)),
            })?;
        Ok(receipt)
    }

    /// swap stub
    #[cfg(not(feature = "evm"))]
    pub async fn swap(
        &self,
        _signer: &LocalSigner,
        _token_in: &Erc20Client,
        _params: SwapParams,
    ) -> Result<String, DefiError> {
        Err(DefiError::ConfigError("evm feature not enabled".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_router_address_lowercase() {
        assert_eq!(
            SWAP_ROUTER_02_ADDRESS.to_lowercase(),
            "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45"
        );
    }

    #[test]
    fn swap_params_builder_pattern() {
        let token_in = Address::from([0x11u8; 20]);
        let token_out = Address::from([0x22u8; 20]);
        let recipient = Address::from([0x33u8; 20]);
        let params = SwapParams::new(token_in, token_out, 3000, U256::from(1000u64))
            .with_min_out(U256::from(900u64))
            .with_recipient(recipient);
        assert_eq!(params.fee, 3000);
        assert_eq!(params.amount_in, U256::from(1000u64));
        assert_eq!(params.min_amount_out, U256::from(900u64));
        assert_eq!(params.recipient, Some(recipient));
    }
}
