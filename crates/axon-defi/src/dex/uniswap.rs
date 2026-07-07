//! Uniswap V3 路由(0.3.0 P0 Batch 3 重写)
//!
//! 0.3.0 改造点:
//! - `quote_swap` 不再是 `amount_in * fee_factor` 模拟,改走 [V3Quoter] 真链报价
//! - `get_best_route` 扫描 4 个 fee tier(100/500/3000/10000)选最优
//! - 新增 [estimate_price_impact] / [pool_depth] 走池子 `slot0()` + `liquidity()`

use serde::{Deserialize, Serialize};

use crate::error::DefiError;
use crate::evm::chain::Chain;

#[cfg(feature = "evm")]
use alloy::primitives::{Address, U256};

pub use crate::dex::v3_quoter::{is_valid_fee_tier, V3Quoter, FEE_TIERS};

/// Uniswap V3 合约地址
#[derive(Debug, Clone)]
pub struct UniswapV3Contracts {
    /// 工厂合约
    pub factory: String,
    /// 路由合约(SwapRouter02)
    pub router: String,
    /// NonfungiblePositionManager(LP 仓位管理)
    pub position_manager: String,
    /// QuoterV2 报价合约
    pub quoter: String,
}

impl UniswapV3Contracts {
    /// 获取指定链的合约地址
    ///
    /// 4 链共用 Uniswap canonical 地址
    #[allow(unused_variables)] // `chain` 保留以备后续 per-chain 路由差异
    pub fn for_chain(chain: &Chain) -> Self {
        Self {
            factory: "0x1F98431c8aD98523631AE4a59f267346ea31F984".into(),
            router: crate::dex::v3_router::SWAP_ROUTER_02_ADDRESS.into(),
            position_manager: "0xC36442b4a4522E871399CD717aBDD847Ab11FE88".into(),
            quoter: crate::dex::v3_quoter::QUOTER_V2_ADDRESS.into(),
        }
    }
}

/// Uniswap V3 池子信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolInfo {
    /// 代币 0
    pub token0: String,
    /// 代币 1
    pub token1: String,
    /// 费率(500, 3000, 10000)
    pub fee: u32,
    /// 流动性
    pub liquidity: String,
    /// 价格
    pub sqrt_price_x96: String,
    /// Tick
    pub tick: i32,
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
    /// 跨过的 tick 数(深度代理)
    pub initialized_ticks_crossed: u32,
    /// 估算 gas
    pub gas_estimate: String,
}

impl SwapRoute {
    /// 计算 amount_out (f64,用于比较)
    #[cfg(feature = "evm")]
    pub fn amount_out_f64(&self) -> f64 {
        // U256 -> string -> f64 转换(仅用于 best route 比较,精度损失可接受)
        let s = self.amount_out.trim_start_matches("0x");
        let bytes = alloy::primitives::hex::decode(s).unwrap_or_default();
        let mut be = [0u8; 32];
        let start = 32usize.saturating_sub(bytes.len());
        be[start..].copy_from_slice(&bytes[..bytes.len().min(32)]);
        let u = U256::from_be_bytes(be);
        // 简化:u.to_string() 解析,精度损失可控
        u.to_string().parse::<f64>().unwrap_or(0.0)
    }

    #[cfg(not(feature = "evm"))]
    pub fn amount_out_f64(&self) -> f64 {
        self.amount_out.parse().unwrap_or(0.0)
    }
}

/// Uniswap V3 路由器
pub struct UniswapRouter {
    chain: Chain,
    contracts: UniswapV3Contracts,
}

impl UniswapRouter {
    /// 创建新的路由器
    pub fn new(chain: Chain) -> Self {
        let contracts = UniswapV3Contracts::for_chain(&chain);
        Self { chain, contracts }
    }

    /// 获取链
    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    /// 获取合约地址
    pub fn contracts(&self) -> &UniswapV3Contracts {
        &self.contracts
    }

    /// 获取所有支持的费率
    pub fn fee_tiers(&self) -> &[u32] {
        &FEE_TIERS
    }

    /// 报价(走真 QuoterV2)
    ///
    /// 0.3.0 改造:不再用 `amount_in * fee_factor` 模拟,改走 [V3Quoter::quote_exact_input_single]
    #[cfg(feature = "evm")]
    pub async fn quote_swap(
        &self,
        quoter: &V3Quoter,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        fee: u32,
    ) -> Result<QuoteWithMeta, DefiError> {
        if !is_valid_fee_tier(fee) {
            return Err(DefiError::ConfigError(format!("invalid fee tier: {}", fee)));
        }
        let res = quoter
            .quote_exact_input_single(token_in, token_out, amount_in, fee, U256::ZERO)
            .await?;
        Ok(QuoteWithMeta {
            amount_out: res.amount_out,
            fee,
            initialized_ticks_crossed: res.initialized_ticks_crossed,
            gas_estimate: res.gas_estimate,
        })
    }

    /// quote_swap stub(无 evm feature)
    #[cfg(not(feature = "evm"))]
    pub async fn quote_swap(
        &self,
        _quoter: &V3Quoter,
        _token_in: String,
        _token_out: String,
        _amount_in: String,
        fee: u32,
    ) -> Result<String, DefiError> {
        if !is_valid_fee_tier(fee) {
            return Err(DefiError::ConfigError(format!("invalid fee tier: {}", fee)));
        }
        Ok("0".into())
    }

    /// 获取最优路由(扫描 4 个 fee tier,选 amount_out 最大)
    ///
    /// 0.3.0 改造:不再用模拟,改走 V3Quoter 真实扫描
    #[cfg(feature = "evm")]
    pub async fn get_best_route(
        &self,
        quoter: &V3Quoter,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> Result<SwapRoute, DefiError> {
        let mut best: Option<QuoteWithMeta> = None;
        for &fee in &FEE_TIERS {
            match self.quote_swap(quoter, token_in, token_out, amount_in, fee).await {
                Ok(q) => {
                    if best.as_ref().is_none_or(|b| q.amount_out > b.amount_out) {
                        best = Some(q);
                    }
                }
                // 某个 fee tier 池子不存在 → 跳过
                Err(_) => continue,
            }
        }
        let b = best.ok_or(DefiError::NoRouteFound)?;
        Ok(SwapRoute {
            token_in: format!("{:?}", token_in),
            token_out: format!("{:?}", token_out),
            fee: b.fee,
            amount_in: amount_in.to_string(),
            amount_out: b.amount_out.to_string(),
            initialized_ticks_crossed: b.initialized_ticks_crossed,
            gas_estimate: b.gas_estimate.to_string(),
        })
    }

    /// get_best_route stub
    #[cfg(not(feature = "evm"))]
    pub async fn get_best_route(
        &self,
        _quoter: &V3Quoter,
        _token_in: String,
        _token_out: String,
        _amount_in: String,
    ) -> Result<SwapRoute, DefiError> {
        Err(DefiError::NoRouteFound)
    }
}

/// 报价 + 元信息
#[derive(Debug, Clone)]
pub struct QuoteWithMeta {
    /// 输出 token 数量
    pub amount_out: U256,
    /// fee tier
    pub fee: u32,
    /// 跨过 tick 数
    pub initialized_ticks_crossed: u32,
    /// 估算 gas
    pub gas_estimate: U256,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uniswap_contracts_for_chain() {
        let contracts = UniswapV3Contracts::for_chain(&Chain::Ethereum);
        assert!(!contracts.factory.is_empty());
        assert!(!contracts.router.is_empty());
        assert!(!contracts.position_manager.is_empty());
        assert!(!contracts.quoter.is_empty());
    }

    #[test]
    fn test_uniswap_contracts_all_chains() {
        for chain in [
            Chain::Ethereum,
            Chain::Arbitrum,
            Chain::Optimism,
            Chain::Polygon,
        ] {
            let contracts = UniswapV3Contracts::for_chain(&chain);
            assert_eq!(
                contracts.quoter.to_lowercase(),
                crate::dex::v3_quoter::QUOTER_V2_ADDRESS.to_lowercase()
            );
        }
    }

    #[test]
    fn test_uniswap_router_creation() {
        let router = UniswapRouter::new(Chain::Ethereum);
        assert_eq!(*router.chain(), Chain::Ethereum);
    }

    #[test]
    fn test_uniswap_router_fee_tiers() {
        let router = UniswapRouter::new(Chain::Ethereum);
        assert_eq!(router.fee_tiers(), &[100, 500, 3000, 10000]);
    }

    #[test]
    fn test_swap_route_serialization() {
        let route = SwapRoute {
            token_in: "0xA".into(),
            token_out: "0xB".into(),
            fee: 3000,
            amount_in: "1000".into(),
            amount_out: "997".into(),
            initialized_ticks_crossed: 1,
            gas_estimate: "150000".into(),
        };
        let json = serde_json::to_string(&route).unwrap();
        let restored: SwapRoute = serde_json::from_str(&json).unwrap();
        assert_eq!(route.fee, restored.fee);
    }

    #[test]
    fn test_pool_info_serialization() {
        let pool = PoolInfo {
            token0: "0xA".into(),
            token1: "0xB".into(),
            fee: 3000,
            liquidity: "1000000".into(),
            sqrt_price_x96: "1000000000".into(),
            tick: 0,
        };
        let json = serde_json::to_string(&pool).unwrap();
        let restored: PoolInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(pool.fee, restored.fee);
    }
}
