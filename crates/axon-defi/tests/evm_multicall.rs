//! Multicall3 集成测试(需要本地 anvil 节点)
//!
//! 0.3.0 P0 Batch 2 / T1.5:验证批量查询通过单次 RPC 完成。

#![cfg(feature = "evm")]

use std::time::Duration;

use alloy_primitives::U256;

use axon_defi::evm::chain::Chain;
use axon_defi::evm::multicall::{Call3, Multicall, Multicall3};
use axon_defi::evm::provider::{EvmProvider, ProviderConfig};

const ANVIL_URL: &str = "http://127.0.0.1:8545";

// vitalik.eth 等多 holder(任选,仅用于 multicall 批量演示)
const VITALIK: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
const HOLDER_A: &str = "0x0000000000000000000000000000000000000001";

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

async fn anvil_running() -> bool {
    matches!(
        tokio::time::timeout(Duration::from_millis(500), reqwest::get(ANVIL_URL)).await,
        Ok(Ok(_))
    )
}

fn provider() -> EvmProvider {
    EvmProvider::new(ProviderConfig::for_chain(Chain::Ethereum, ANVIL_URL))
}

// ---------- 纯单测(不连真链) ----------

#[test]
fn multicall3_address_is_canonical() {
    // Multicall3 在 mainnet 和几乎所有 EVM 链上同一地址
    assert_eq!(
        Multicall3::CANONICAL_ADDRESS.to_lowercase(),
        "0xca11bde05977b3631167028862be2a173976ca11"
    );
}

#[test]
fn multicall3_supports_chain_ethereum() {
    // 0xca11bde05977b3631167028862be2a173976ca11 在 Ethereum mainnet 部署
    assert!(Multicall3::is_deployed_on(Chain::Ethereum));
}

#[test]
fn multicall3_supports_chain_arbitrum() {
    assert!(Multicall3::is_deployed_on(Chain::Arbitrum));
}

#[test]
fn multicall3_supports_chain_optimism() {
    assert!(Multicall3::is_deployed_on(Chain::Optimism));
}

#[test]
fn multicall3_supports_chain_polygon() {
    assert!(Multicall3::is_deployed_on(Chain::Polygon));
}

#[test]
fn call3_constructs_with_allow_failure_true() {
    let call = Call3::new(USDC, "0x70a08231"); // balanceOf(address) selector
    assert!(call.allow_failure);
    assert_eq!(call.target.to_lowercase(), USDC.to_lowercase());
    assert!(!call.call_data.is_empty());
}

#[test]
fn call3_constructs_strict() {
    // strict:失败立即终止
    let call = Call3::strict(USDC, "0x70a08231");
    assert!(!call.allow_failure);
}

#[test]
fn call3_serializes_roundtrip() {
    let call = Call3::new(USDC, "0x70a08231");
    let json = serde_json::to_string(&call).unwrap();
    let restored: Call3 = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.target.to_lowercase(), call.target.to_lowercase());
    assert_eq!(restored.allow_failure, call.allow_failure);
}

#[test]
fn multicall_constructs_with_provider() {
    let mc = Multicall::new(provider(), Chain::Ethereum);
    assert_eq!(mc.chain(), Chain::Ethereum);
}

#[test]
fn multicall_address_for_chain_ethereum() {
    let mc = Multicall::new(provider(), Chain::Ethereum);
    assert_eq!(
        mc.address().to_lowercase(),
        "0xca11bde05977b3631167028862be2a173976ca11"
    );
}

// ---------- anvil fork 集成测试(无 anvil 自动 skip) ----------

#[tokio::test]
async fn multicall_aggregate3_single_call() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let mc = Multicall::new(provider(), Chain::Ethereum);
    // balanceOf(vitalik) 在 USDC
    let balance_selector = alloy::primitives::hex::decode("70a08231").unwrap();
    let mut data = balance_selector.clone();
    // padded address
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(
        alloy_primitives::Address::parse_checksummed(VITALIK, None)
            .unwrap()
            .as_slice(),
    );

    let calls = vec![Call3::strict(
        USDC,
        &format!("0x{}", alloy::primitives::hex::encode(&data)),
    )];
    let results = mc.aggregate3(calls).await.expect("aggregate3 ok");
    assert_eq!(results.len(), 1);
    assert!(results[0].success, "single call should succeed");
    assert!(!results[0].return_data.is_empty());
}

#[tokio::test]
async fn multicall_balance_of_batch_100_holders() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let mc = Multicall::new(provider(), Chain::Ethereum);
    // 构造 100 个 holder 的 balanceOf 调用
    let holders: Vec<alloy_primitives::Address> = (0..100)
        .map(|i| {
            let mut bytes = [0u8; 20];
            bytes[19] = i as u8;
            alloy_primitives::Address::from(bytes)
        })
        .collect();

    let balances = mc
        .balance_of_batch(USDC, &holders)
        .await
        .expect("batch balance_of ok");

    assert_eq!(balances.len(), 100);
    // 100 个 0x00..0x{0..99} 应当余额都为 0(非真实地址)
    for (i, b) in balances.iter().enumerate() {
        assert_eq!(*b, U256::ZERO, "holder #{} should have zero balance", i);
    }
}

#[tokio::test]
async fn multicall_balance_of_isolates_failures() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let mc = Multicall::new(provider(), Chain::Ethereum);
    // 1 个真地址(vitalik) + 1 个伪地址
    let holders = vec![
        alloy_primitives::Address::parse_checksummed(VITALIK, None).unwrap(),
        alloy_primitives::Address::parse_checksummed(HOLDER_A, None).unwrap(),
    ];
    let balances = mc
        .balance_of_batch(USDC, &holders)
        .await
        .expect("batch balance_of ok");
    assert_eq!(balances.len(), 2);
    // vitalik 应有 USDC 余额(>0)
    assert!(
        balances[0] > U256::ZERO,
        "vitalik USDC balance should be > 0, got {}",
        balances[0]
    );
    // HOLDER_A 应为 0
    assert_eq!(balances[1], U256::ZERO);
}

#[tokio::test]
async fn multicall_decimals_batch_returns_6_for_usdc() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let mc = Multicall::new(provider(), Chain::Ethereum);
    // USDC decimals = 6, USDT decimals = 6
    let usdc = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
    let usdt = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
    let tokens = vec![usdc, usdt];
    let decimals_list = mc.decimals_batch(&tokens).await.expect("decimals batch ok");
    assert_eq!(decimals_list.len(), 2);
    assert_eq!(decimals_list[0], 6);
    assert_eq!(decimals_list[1], 6);
}
