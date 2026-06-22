//! Uniswap V3 路由

use serde::{Deserialize, Serialize};

use crate::error::DefiError;
use crate::evm::chain::Chain;

/// Uniswap V3 合约地址
#[derive(Debug, Clone)]
pub struct UniswapV3Contracts {
    /// 工厂合约
    pub factory: String,
    /// 路由合约（SwapRouter）
    pub router: String,
    /// 非同质化仓位管理
    pub position_manager: String,
}

impl UniswapV3Contracts {
    /// 获取指定链的合约地址
    pub fn for_chain(chain: &Chain) -> Self {
        match chain {
            Chain::Ethereum => Self {
                factory: "0x1F98431c8aD98523631AE4a59f267346ea31F984".into(),
                router: "0xE592427A0AEce92De3Edee1F18E0157C05861564".into(),
                position_manager: "0xC36442b4a4522E871399CD717aBDD847Ab11FE88".into(),
            },
            Chain::Arbitrum => Self {
                factory: "0x1F98431c8aD98523631AE4a59f267346ea31F984".into(),
                router: "0xE592427A0AEce92De3Edee1F18E0157C05861564".into(),
                position_manager: "0xC36442b4a4522E871399CD717aBDD847Ab11FE88".into(),
            },
            Chain::Optimism => Self {
                factory: "0x1F98431c8aD98523631AE4a59f267346ea31F984".into(),
                router: "0xE592427A0AEce92De3Edee1F18E0157C05861564".into(),
                position_manager: "0xC36442b4a4522E871399CD717aBDD847Ab11FE88".into(),
            },
            Chain::Polygon => Self {
                factory: "0x1F98431c8aD98523631AE4a59f267346ea31F984".into(),
                router: "0xE592427A0AEce92De3Edee1F18E0157C05861564".into(),
                position_manager: "0xC36442b4a4522E871399CD717aBDD847Ab11FE88".into(),
            },
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
    /// 费率（500, 3000, 10000）
    pub fee: u32,
    /// 流动性
    pub liquidity: String,
    /// 价格
    pub sqrt_price_x96: String,
    /// Tick
    pub tick: i32,
}

/// 支持的费率
pub const FEE_TIERS: [u32; 4] = [100, 500, 3000, 10000];

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

    /// 报价（模拟）
    pub async fn quote_swap(
        &self,
        token_in: &str,
        token_out: &str,
        amount_in: &str,
        fee: u32,
    ) -> Result<String, DefiError> {
        // 验证费率
        if !FEE_TIERS.contains(&fee) {
            return Err(DefiError::ConfigError(format!("Invalid fee tier: {}", fee)));
        }

        // 验证代币地址
        if token_in.is_empty() || token_out.is_empty() {
            return Err(DefiError::ConfigError("Token address is empty".into()));
        }

        // 模拟报价（实际实现需要调用合约）
        let amount_in_val: f64 = amount_in.parse().unwrap_or(0.0);
        let fee_factor = 1.0 - (fee as f64 / 1_000_000.0);
        let amount_out = amount_in_val * fee_factor;

        Ok(format!("{:.6}", amount_out))
    }

    /// 获取最优路由
    pub async fn get_best_route(
        &self,
        token_in: &str,
        token_out: &str,
        amount_in: &str,
    ) -> Result<SwapRoute, DefiError> {
        let mut best_route = None;
        let mut best_amount_out = 0.0;

        for &fee in &FEE_TIERS {
            let amount_out_str = self.quote_swap(token_in, token_out, amount_in, fee).await?;
            let amount_out: f64 = amount_out_str.parse().unwrap_or(0.0);

            if amount_out > best_amount_out {
                best_amount_out = amount_out;
                best_route = Some(SwapRoute {
                    token_in: token_in.to_string(),
                    token_out: token_out.to_string(),
                    fee,
                    amount_in: amount_in.to_string(),
                    amount_out: amount_out_str,
                });
            }
        }

        best_route.ok_or(DefiError::NoRouteFound)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uniswap_contracts_for_chain() {
        let contracts = UniswapV3Contracts::for_chain(&Chain::Ethereum);
        assert!(!contracts.factory.is_empty());
        assert!(!contracts.router.is_empty());
        assert!(!contracts.position_manager.is_empty());
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
            assert!(!contracts.factory.is_empty());
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

    #[tokio::test]
    async fn test_uniswap_router_quote_swap() {
        let router = UniswapRouter::new(Chain::Ethereum);
        let result = router.quote_swap("0xA", "0xB", "1000", 3000).await;
        assert!(result.is_ok());
        let amount_out = result.unwrap();
        let amount: f64 = amount_out.parse().unwrap();
        assert!(amount > 0.0);
        assert!(amount < 1000.0); // 扣除手续费
    }

    #[tokio::test]
    async fn test_uniswap_router_quote_invalid_fee() {
        let router = UniswapRouter::new(Chain::Ethereum);
        let result = router.quote_swap("0xA", "0xB", "1000", 999).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_uniswap_router_quote_empty_token() {
        let router = UniswapRouter::new(Chain::Ethereum);
        let result = router.quote_swap("", "0xB", "1000", 3000).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_uniswap_router_get_best_route() {
        let router = UniswapRouter::new(Chain::Ethereum);
        let result = router.get_best_route("0xA", "0xB", "1000").await;
        assert!(result.is_ok());
        let route = result.unwrap();
        assert_eq!(route.token_in, "0xA");
        assert_eq!(route.token_out, "0xB");
        assert!(FEE_TIERS.contains(&route.fee));
    }

    #[test]
    fn test_swap_route_serialization() {
        let route = SwapRoute {
            token_in: "0xA".into(),
            token_out: "0xB".into(),
            fee: 3000,
            amount_in: "1000".into(),
            amount_out: "997".into(),
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
