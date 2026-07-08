//! V3Pool 集成测试(需要本地 anvil 节点)
//!
//! 0.3.0 P0 Batch 3 / T1.10:验证池子 slot0 + liquidity 真链查询

#![cfg(feature = "evm")]

use std::time::Duration;

use axon_defi::dex::v3_pool::V3Pool;
use axon_defi::evm::chain::Chain;
use axon_defi::evm::provider::{EvmProvider, ProviderConfig};

const ANVIL_URL: &str = "http://127.0.0.1:8545";

// USDC/WETH 0.05% pool (canonical)
const USDC_WETH_POOL: &str = "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640";

async fn anvil_running() -> bool {
    matches!(
        tokio::time::timeout(Duration::from_millis(500), reqwest::get(ANVIL_URL)).await,
        Ok(Ok(_))
    )
}

fn provider() -> EvmProvider {
    EvmProvider::new(ProviderConfig::for_chain(Chain::Ethereum, ANVIL_URL))
}

// ---------- 纯单测 ----------

#[test]
fn v3_pool_constructs_with_lowercase_address() {
    let pool = V3Pool::new(
        provider(),
        Chain::Ethereum,
        "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
    );
    assert_eq!(pool.address(), "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640");
}

// ---------- anvil 集成测试 ----------

#[tokio::test]
async fn slot0_returns_positive_price() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let pool = V3Pool::new(provider(), Chain::Ethereum, USDC_WETH_POOL);
    let s = pool.slot0().await.expect("slot0 ok");
    // USDC/WETH 价格应 > 1
    assert!(
        s.sqrt_price_x96 > alloy::primitives::U256::ZERO,
        "sqrtPriceX96 should be > 0"
    );
}

#[tokio::test]
async fn liquidity_returns_positive() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let pool = V3Pool::new(provider(), Chain::Ethereum, USDC_WETH_POOL);
    let liq = pool.liquidity().await.expect("liquidity ok");
    // USDC/WETH pool 流动性应是天文数字
    assert!(
        liq > alloy::primitives::U256::ZERO,
        "liquidity should be > 0"
    );
}

#[tokio::test]
async fn state_returns_combined() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let pool = V3Pool::new(provider(), Chain::Ethereum, USDC_WETH_POOL);
    let s = pool.state().await.expect("state ok");
    assert!(s.sqrt_price_x96 > alloy::primitives::U256::ZERO);
    assert!(s.liquidity > alloy::primitives::U256::ZERO);
}
