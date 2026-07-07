//! V3Quoter 集成测试(需要本地 anvil 节点)
//!
//! 0.3.0 P0 Batch 3 / T1.7:验证 QuoterV2 真链报价

#![cfg(feature = "evm")]

use std::time::Duration;

use alloy_primitives::{Address, U256};

use axon_defi::dex::v3_quoter::{is_valid_fee_tier, V3Quoter, FEE_TIERS};
use axon_defi::evm::chain::Chain;
use axon_defi::evm::provider::{EvmProvider, ProviderConfig};

const ANVIL_URL: &str = "http://127.0.0.1:8545";

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";

async fn anvil_running() -> bool {
    match tokio::time::timeout(
        Duration::from_millis(500),
        reqwest::get(ANVIL_URL),
    )
    .await
    {
        Ok(Ok(_)) => true,
        _ => false,
    }
}

fn provider() -> EvmProvider {
    EvmProvider::new(ProviderConfig::for_chain(Chain::Ethereum, ANVIL_URL))
}

// ---------- 纯单测 ----------

#[test]
fn v3_quoter_constructs_with_default_address() {
    let q = V3Quoter::new(provider(), Chain::Ethereum);
    assert_eq!(
        q.address().to_lowercase(),
        "0x61ffe014ba17989e743c5f6cb21bf9697530b56e"
    );
    assert_eq!(q.chain(), Chain::Ethereum);
}

#[test]
fn v3_quoter_with_custom_address() {
    let q = V3Quoter::new(provider(), Chain::Polygon)
        .with_quoter_address("0x61fFE014bA17989E743c5F6cB21bF9697530B56e");
    // 验证 lowercase 化
    assert_eq!(
        q.address(),
        "0x61ffe014ba17989e743c5f6cb21bf9697530b56e"
    );
}

#[test]
fn fee_tiers_constant() {
    assert_eq!(FEE_TIERS, [100, 500, 3000, 10000]);
}

#[test]
fn valid_fee_tier_passes() {
    for fee in [100u32, 500, 3000, 10000] {
        assert!(is_valid_fee_tier(fee));
    }
}

#[test]
fn invalid_fee_tier_rejected() {
    assert!(!is_valid_fee_tier(0));
    assert!(!is_valid_fee_tier(250));
    assert!(!is_valid_fee_tier(3001));
    assert!(!is_valid_fee_tier(100_000));
}

// ---------- anvil 集成测试 ----------

#[tokio::test]
async fn quote_usdc_to_weth_3000bps_returns_positive() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let q = V3Quoter::new(provider(), Chain::Ethereum);
    let usdc = Address::parse_checksummed(USDC, None).unwrap();
    let weth = Address::parse_checksummed(WETH, None).unwrap();
    // 1000 USDC = 1_000_000_000 (6 decimals)
    let amount_in = U256::from(1_000_000_000u64);

    let res = q
        .quote_exact_input_single(usdc, weth, amount_in, 3000, U256::ZERO)
        .await
        .expect("quote ok");

    // 1000 USDC 应当换到 > 0 WETH(实际 ~0.3 WETH in 2024 prices)
    assert!(res.amount_out > U256::ZERO, "amount_out should be > 0");
    // 滑点 < 0.3%:output 应大于 amount_in * 0.001 (WETH ~$3000/USDC $1)
    // 这里只粗略校验 amount_out 不为 0
    println!(
        "1000 USDC -> {} WETH wei, gas_estimate={}, ticks_crossed={}",
        res.amount_out, res.gas_estimate, res.initialized_ticks_crossed
    );
}

#[tokio::test]
async fn quote_weth_to_usdc_3000bps_returns_positive() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let q = V3Quoter::new(provider(), Chain::Ethereum);
    let usdc = Address::parse_checksummed(USDC, None).unwrap();
    let weth = Address::parse_checksummed(WETH, None).unwrap();
    // 1 WETH = 10^18 wei
    let amount_in = U256::from(10u64).pow(U256::from(18u64));

    let res = q
        .quote_exact_input_single(weth, usdc, amount_in, 3000, U256::ZERO)
        .await
        .expect("quote ok");

    // 1 WETH 应当换到 > 1000 USDC(按价格 3000+ USDC/WETH)
    // 1000 USDC = 10^9 (6 decimals)
    assert!(
        res.amount_out > U256::from(1_000_000_000u64),
        "1 WETH should swap to > 1000 USDC, got {}",
        res.amount_out
    );
}

#[tokio::test]
async fn quote_different_fee_tiers_yields_different_results() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let q = V3Quoter::new(provider(), Chain::Ethereum);
    let usdc = Address::parse_checksummed(USDC, None).unwrap();
    let weth = Address::parse_checksummed(WETH, None).unwrap();
    let amount_in = U256::from(10u64).pow(U256::from(18u64));

    let res_3000 = q
        .quote_exact_input_single(weth, usdc, amount_in, 3000, U256::ZERO)
        .await
        .expect("3000bps quote ok");
    let res_10000 = q
        .quote_exact_input_single(weth, usdc, amount_in, 10000, U256::ZERO)
        .await
        .expect("10000bps quote ok");

    // 10000 bps(1%) 比 3000 bps(0.3%) 贵 → 同样 WETH 换到的 USDC 更少
    // (因为 1% pool 价格更差,且流动性可能更差)
    assert!(
        res_10000.amount_out <= res_3000.amount_out,
        "10000bps should yield <= 3000bps, got {} vs {}",
        res_10000.amount_out,
        res_3000.amount_out
    );
}

#[tokio::test]
async fn quote_unknown_pool_returns_zero_or_error() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let q = V3Quoter::new(provider(), Chain::Ethereum);
    // 任意不存在的 token
    let fake_token = Address::from([0x42u8; 20]);
    let weth = Address::parse_checksummed(WETH, None).unwrap();
    let amount_in = U256::from(1_000_000_000u64);

    // 池子不存在 → QuoterV2 revert → 我们要捕获
    let res = q
        .quote_exact_input_single(fake_token, weth, amount_in, 3000, U256::ZERO)
        .await;
    assert!(res.is_err(), "unknown pool should error, got {:?}", res);
}
