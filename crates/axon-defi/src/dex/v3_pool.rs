//! Uniswap V3 池子查询(slot0 + liquidity)
//!
//! 0.3.0 P0 Batch 3 / T1.10:封装 `IUniswapV3Pool` 合约,提供:
//! - `slot0()` 拿 sqrtPriceX96 + tick(链上当前价)
//! - `liquidity()` 拿当前 tick 范围内的流动性总量
//!
//! 注意:本模块不调工厂计算 pool address,而是接受 caller 提供的 pool 地址
//! (生产场景:factories 已知 + 用 `getPool(tokenA, tokenB, fee)` 计算)。

use crate::evm::chain::Chain;
use crate::evm::provider::EvmProvider;

#[cfg(feature = "evm")]
use crate::error::DefiError;
#[cfg(feature = "evm")]
use alloy::network::TransactionBuilder;
#[cfg(feature = "evm")]
use alloy::primitives::{Address, U256};

/// V3 池子 slot0 返回
#[cfg(feature = "evm")]
#[derive(Debug, Clone)]
pub struct Slot0 {
    /// sqrtPriceX96(价)
    pub sqrt_price_x96: U256,
    /// tick
    pub tick: i32,
}

/// V3 池子状态
#[cfg(feature = "evm")]
#[derive(Debug, Clone)]
pub struct PoolState {
    /// sqrtPriceX96(价)
    pub sqrt_price_x96: U256,
    /// tick
    pub tick: i32,
    /// 当前流动性
    pub liquidity: U256,
}

/// V3 池子客户端
#[derive(Clone)]
pub struct V3Pool {
    provider: EvmProvider,
    pool_address: String,
    chain: Chain,
}

impl V3Pool {
    /// 构造
    pub fn new(provider: EvmProvider, chain: Chain, pool_address: impl Into<String>) -> Self {
        Self {
            provider,
            pool_address: pool_address.into().to_lowercase(),
            chain,
        }
    }

    /// 池子地址
    pub fn address(&self) -> String {
        self.pool_address.clone()
    }

    /// 关联链
    pub fn chain(&self) -> Chain {
        self.chain
    }

    /// 读 slot0(sqrtPriceX96, tick, ...)
    #[cfg(feature = "evm")]
    pub async fn slot0(&self) -> Result<Slot0, DefiError> {
        use alloy::providers::Provider;
        use alloy::sol;
        use alloy::sol_types::SolCall;
        sol! {
            #[sol(rpc)]
            interface IUniswapV3Pool {
                function slot0()
                    external view
                    returns (
                        uint160 sqrtPriceX96,
                        int24 tick,
                        uint16 observationIndex,
                        uint16 observationCardinality,
                        uint16 observationCardinalityNext,
                        uint32 feeProtocol,
                        bool unlocked
                    );
            }
        }

        let pool: Address = self
            .pool_address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid pool address: {}", e)))?;
        let parsed_url = ::url::Url::parse(&self.provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new().connect_http(parsed_url);

        // 直接用 raw eth_call:slot0() selector
        let call = IUniswapV3Pool::slot0Call {};
        let input = call.abi_encode();
        let tx = alloy::rpc::types::TransactionRequest::default()
            .with_to(pool)
            .with_input(input);
        let output: alloy::primitives::Bytes =
            p.call(tx).await.map_err(|e| DefiError::RpcError {
                url: self.provider.config().rpc_url.clone(),
                status: 0,
                body: DefiError::truncated_body(&format!("{}", e)),
            })?;

        // 解码 (uint160, int24, uint16, uint16, uint16, uint32, bool) tuple
        type Slot0Return = (U256, i32, u16, u16, u16, u32, bool);
        let ret: Slot0Return = alloy::sol_types::SolValue::abi_decode(&output).map_err(|e| {
            DefiError::ContractError {
                address: self.pool_address.clone(),
                method: "slot0".into(),
                reason: format!("decode: {}", e),
            }
        })?;
        let (sqrt_price_x96, tick, _, _, _, _, _) = ret;
        // uint160 -> U256 转换
        let sqrt_price_x96_u256 = {
            let be_bytes: [u8; 32] = sqrt_price_x96.to_be_bytes();
            let mut padded = [0u8; 32];
            padded[12..32].copy_from_slice(&be_bytes[12..32]);
            U256::from_be_bytes(padded)
        };
        Ok(Slot0 {
            sqrt_price_x96: sqrt_price_x96_u256,
            tick,
        })
    }

    /// 读 liquidity()
    #[cfg(feature = "evm")]
    pub async fn liquidity(&self) -> Result<U256, DefiError> {
        use alloy::providers::Provider;
        use alloy::sol;
        use alloy::sol_types::SolCall;
        sol! {
            #[sol(rpc)]
            interface IUniswapV3PoolLiquidity {
                function liquidity() external view returns (uint128);
            }
        }

        let pool: Address = self
            .pool_address
            .parse()
            .map_err(|e| DefiError::ConfigError(format!("invalid pool address: {}", e)))?;
        let parsed_url = ::url::Url::parse(&self.provider.config().rpc_url)
            .map_err(|e| DefiError::ConfigError(format!("invalid url: {}", e)))?;
        let p = alloy::providers::ProviderBuilder::new().connect_http(parsed_url);

        let call = IUniswapV3PoolLiquidity::liquidityCall {};
        let input = call.abi_encode();
        let tx = alloy::rpc::types::TransactionRequest::default()
            .with_to(pool)
            .with_input(input);
        let output: alloy::primitives::Bytes =
            p.call(tx).await.map_err(|e| DefiError::RpcError {
                url: self.provider.config().rpc_url.clone(),
                status: 0,
                body: DefiError::truncated_body(&format!("{}", e)),
            })?;

        // uint128 编码到 32 字节
        if output.len() < 32 {
            return Err(DefiError::ContractError {
                address: self.pool_address.clone(),
                method: "liquidity".into(),
                reason: "output too short".into(),
            });
        }
        Ok(U256::from_be_slice(&output[..32]))
    }

    /// 读 slot0 + liquidity 一并(单次 multi-call 简化版,实际分别发两次 RPC)
    #[cfg(feature = "evm")]
    pub async fn state(&self) -> Result<PoolState, DefiError> {
        let s = self.slot0().await?;
        let liq = self.liquidity().await?;
        Ok(PoolState {
            sqrt_price_x96: s.sqrt_price_x96,
            tick: s.tick,
            liquidity: liq,
        })
    }

    /// 估算价格冲击
    ///
    /// 简化版:基于 quote 输出 vs 1:1 基准的比值
    /// `(expected_out_no_slippage - actual_out) / expected_out_no_slippage`
    ///
    /// 当前 token 价格 = 1 (简化假设);调用方可在生产前用 oracle 价格校正
    #[cfg(feature = "evm")]
    pub fn estimate_price_impact(&self, amount_in: U256, amount_out: U256) -> f64 {
        if amount_in.is_zero() {
            return 0.0;
        }
        // 简化:amount_in * 1.0 作为 baseline(amount_out for no-slippage)
        // price_impact = 1 - amount_out / amount_in
        // 精度损失可接受(仅作 routing 决策参考)
        let in_f = amount_in.to_string().parse::<f64>().unwrap_or(0.0);
        let out_f = amount_out.to_string().parse::<f64>().unwrap_or(0.0);
        if in_f == 0.0 {
            0.0
        } else {
            (1.0 - out_f / in_f).max(0.0)
        }
    }

    /// estimate_price_impact stub(evm feature 关闭时)
    #[cfg(not(feature = "evm"))]
    pub fn estimate_price_impact(&self, _amount_in: u128, _amount_out: u128) -> f64 {
        0.0
    }
}

#[cfg(all(test, feature = "evm"))]
mod tests {
    use super::*;
    use crate::evm::provider::ProviderConfig;

    #[test]
    fn v3_pool_constructs_with_address() {
        let provider = EvmProvider::new(ProviderConfig::for_chain(
            Chain::Ethereum,
            "http://localhost:8545",
        ));
        let pool = V3Pool::new(
            provider,
            Chain::Ethereum,
            "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640", // USDC/WETH 0.05%
        );
        assert_eq!(pool.address(), "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640");
        assert_eq!(pool.chain(), Chain::Ethereum);
    }

    #[test]
    fn price_impact_zero_input_returns_zero() {
        let provider = EvmProvider::new(ProviderConfig::for_chain(
            Chain::Ethereum,
            "http://localhost:8545",
        ));
        let pool = V3Pool::new(
            provider,
            Chain::Ethereum,
            "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
        );
        let impact = pool.estimate_price_impact(U256::ZERO, U256::ZERO);
        assert_eq!(impact, 0.0);
    }

    #[test]
    fn price_impact_normal_calc() {
        let provider = EvmProvider::new(ProviderConfig::for_chain(
            Chain::Ethereum,
            "http://localhost:8545",
        ));
        let pool = V3Pool::new(
            provider,
            Chain::Ethereum,
            "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
        );
        // 假设 1000 in 换 997 out(0.3% 手续费 + 0% 滑点)
        let impact = pool.estimate_price_impact(U256::from(1000u64), U256::from(997u64));
        // 0.3% 价格冲击 (简化)
        assert!(impact > 0.0 && impact < 0.01, "got {}", impact);
    }
}
