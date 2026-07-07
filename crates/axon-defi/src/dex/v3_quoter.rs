//! Uniswap V3 QuoterV2 真实报价
//!
//! 0.3.0 P0 Batch 3 / T1.7:封装 `IQuoterV2` 合约,提供真实链上 quote,
//! 替换原 `uniswap.rs::quote_swap` 中的 `amount_in * fee_factor` 模拟实现。
//!
//! 关键设计:
//! - QuoterV2 没有 `view` 修饰,只能通过 `eth_call` 模拟发送交易拿返回
//! - 函数 `quoteExactInputSingle(QuoteExactInputSingleParams) -> QuoteExactInputSingleReturn`
//! - 返回:amountOut + sqrtPriceX96After + initializedTicksCrossed + gasEstimate

use crate::error::DefiError;
use crate::evm::chain::Chain;
use crate::evm::provider::EvmProvider;

#[cfg(feature = "evm")]
use alloy::primitives::{Address, U160, U256};
#[cfg(feature = "evm")]
use alloy::providers::Provider;
#[cfg(feature = "evm")]
use alloy::rpc::types::TransactionRequest;
#[cfg(feature = "evm")]
use alloy::sol;
#[cfg(feature = "evm")]
use alloy::sol_types::{SolCall, SolValue};
#[cfg(feature = "evm")]
use alloy::network::TransactionBuilder;

/// QuoterV2 合约地址
///
/// mainnet/Arbitrum/Optimism/Polygon 同一地址(Canonical deployment by Uniswap)
pub const QUOTER_V2_ADDRESS: &str = "0x61fFE014bA17989E743c5F6cB21bF9697530B56e";

#[cfg(feature = "evm")]
sol! {
    #[sol(rpc)]
    interface IQuoterV2 {
        struct QuoteExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint256 amountIn;
            uint24 fee;
            uint160 sqrtPriceLimitX96;
        }
        function quoteExactInputSingle(QuoteExactInputSingleParams params)
            external
            returns (
                uint256 amountOut,
                uint160 sqrtPriceX96After,
                uint32 initializedTicksCrossed,
                uint256 gasEstimate
            );
    }
}

/// 报价结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteResult {
    /// 输出 token 数量
    pub amount_out: U256,
    /// 报价后 sqrtPriceX96
    pub sqrt_price_x96_after: U256,
    /// 跨过的 initialized tick 数
    pub initialized_ticks_crossed: u32,
    /// 估算 gas
    pub gas_estimate: U256,
}

/// V3 Quoter 客户端
#[derive(Debug, Clone)]
pub struct V3Quoter {
    provider: EvmProvider,
    /// QuoterV2 合约地址(留作可覆盖,默认 [QUOTER_V2_ADDRESS])
    quoter_address: String,
    /// 关联链
    chain: Chain,
}

impl V3Quoter {
    /// 构造(默认 [QUOTER_V2_ADDRESS])
    pub fn new(provider: EvmProvider, chain: Chain) -> Self {
        Self {
            provider,
            quoter_address: QUOTER_V2_ADDRESS.to_string(),
            chain,
        }
    }

    /// 自定义 quoter 地址(测试用)
    pub fn with_quoter_address(mut self, addr: impl Into<String>) -> Self {
        self.quoter_address = addr.into().to_lowercase();
        self
    }

    /// Quoter 合约地址
    pub fn address(&self) -> String {
        self.quoter_address.clone()
    }

    /// 关联链
    pub fn chain(&self) -> Chain {
        self.chain
    }

    /// 走 QuoterV2 拿真实 quote
    ///
    /// `sqrt_price_limit_x96 = 0` 表示不限价
    #[cfg(feature = "evm")]
    pub async fn quote_exact_input_single(
        &self,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        fee: u32,
        sqrt_price_limit_x96: U256,
    ) -> Result<QuoteResult, DefiError> {
        let quoter: Address = self
            .quoter_address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid quoter address: {}", e)))?;
        let parsed_url = ::url::Url::parse(&self.provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new().connect_http(parsed_url);

        // U256 -> U160 截断(取低 160 位)
        let sqrt_price_limit_x160: U160 = {
            let be_bytes: [u8; 32] = sqrt_price_limit_x96.to_be_bytes();
            let mut a = [0u8; 20];
            a.copy_from_slice(&be_bytes[12..32]);
            U160::from_be_bytes(a)
        };

        let call = IQuoterV2::quoteExactInputSingleCall {
            params: IQuoterV2::QuoteExactInputSingleParams {
                tokenIn: token_in,
                tokenOut: token_out,
                amountIn: amount_in,
                fee: {
                    use alloy::primitives::Uint;
                    Uint::<24, 1>::try_from(fee).map_err(|e| {
                        DefiError::ConfigError(format!("fee out of range: {}", e))
                    })?
                },
                sqrtPriceLimitX96: sqrt_price_limit_x160,
            },
        };
        let input = call.abi_encode();
        let tx = TransactionRequest::default()
            .with_to(quoter)
            .with_input(input);
        let output: alloy::primitives::Bytes =
            p.call(tx).await.map_err(|e| DefiError::RpcError {
                url: self.provider.config().rpc_url.clone(),
                status: 0,
                body: DefiError::truncated_body(&format!("{}", e)),
            })?;

        // alloy 1.6:解 ABI 编码的 (U256, U160, u32, U256) tuple
        type QuoteReturn = (U256, U160, u32, U256);
        let ret: QuoteReturn = SolValue::abi_decode(&output).map_err(|e| {
            DefiError::ContractError {
                address: self.quoter_address.clone(),
                method: "quoteExactInputSingle".into(),
                reason: format!("decode: {}", e),
            }
        })?;
        let (amount_out, sqrt_price_x160, ticks, gas) = ret;

        // U160 -> U256 转换(用于对外统一类型)
        let sqrt_price_x96_after: U256 = {
            let bytes: [u8; 20] = sqrt_price_x160.to_be_bytes();
            let mut padded = [0u8; 32];
            padded[12..32].copy_from_slice(&bytes);
            U256::from_be_bytes(padded)
        };

        Ok(QuoteResult {
            amount_out,
            sqrt_price_x96_after,
            initialized_ticks_crossed: ticks,
            gas_estimate: gas,
        })
    }

    /// quote stub
    #[cfg(not(feature = "evm"))]
    pub async fn quote_exact_input_single(
        &self,
        _token_in: String,
        _token_out: String,
        _amount_in: String,
        _fee: u32,
    ) -> Result<String, DefiError> {
        Err(DefiError::ConfigError("evm feature not enabled".into()))
    }
}

/// Uniswap V3 支持的 fee tier(pips)
pub const FEE_TIERS: [u32; 4] = [100, 500, 3000, 10000];

/// 校验 fee tier 是否合法
pub fn is_valid_fee_tier(fee: u32) -> bool {
    FEE_TIERS.contains(&fee)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_quoter_address_lowercase() {
        assert_eq!(
            QUOTER_V2_ADDRESS.to_lowercase(),
            "0x61ffe014ba17989e743c5f6cb21bf9697530b56e"
        );
    }

    #[test]
    fn fee_tiers_canonical() {
        assert_eq!(FEE_TIERS, [100, 500, 3000, 10000]);
    }

    #[test]
    fn fee_validation_accepts_known_tiers() {
        for fee in FEE_TIERS {
            assert!(is_valid_fee_tier(fee));
        }
    }

    #[test]
    fn fee_validation_rejects_unknown_tiers() {
        assert!(!is_valid_fee_tier(0));
        assert!(!is_valid_fee_tier(999));
        assert!(!is_valid_fee_tier(3000 + 1));
    }
}
