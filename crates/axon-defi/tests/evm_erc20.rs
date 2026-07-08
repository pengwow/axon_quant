//! Erc20Client 集成测试(需要本地 anvil 节点)

#![cfg(feature = "evm")]

use std::time::Duration;

use axon_defi::evm::chain::Chain;
use axon_defi::evm::erc20::{Erc20Client, TokenInfo};
use axon_defi::evm::provider::ProviderConfig;

const ANVIL_URL: &str = "http://127.0.0.1:8545";

// anvil fork mainnet 的 USDC / WETH 地址
const USDC_ADDR: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const WETH_ADDR: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";

// vitalik.eth 地址(mainnet 上有多个 token 余额)
const VITALIK_ADDR: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";

async fn anvil_running() -> bool {
    matches!(
        tokio::time::timeout(Duration::from_millis(500), reqwest::get(ANVIL_URL.to_string())).await,
        Ok(Ok(_))
    )
}

fn provider() -> axon_defi::evm::provider::EvmProvider {
    axon_defi::evm::provider::EvmProvider::new(ProviderConfig::for_chain(
        Chain::Ethereum,
        ANVIL_URL,
    ))
}

#[tokio::test]
async fn erc20_client_constructs_for_address() {
    // 期望:能从地址构造 Erc20Client
    let client = Erc20Client::new(USDC_ADDR, provider());
    let info = client.info();
    assert_eq!(info.address, USDC_ADDR.to_lowercase());
}

#[tokio::test]
async fn erc20_client_usdc_decimals_returns_6() {
    // 期望:USDC decimals = 6
    let client = Erc20Client::new(USDC_ADDR, provider());
    let d = client
        .decimals()
        .await
        .expect("USDC decimals should succeed");
    assert_eq!(d, 6);
}

#[tokio::test]
async fn erc20_client_usdc_symbol_returns_usdc() {
    // 期望:USDC symbol = "USDC"
    let client = Erc20Client::new(USDC_ADDR, provider());
    let s = client.symbol().await.expect("USDC symbol should succeed");
    assert_eq!(s, "USDC");
}

#[tokio::test]
async fn erc20_client_weth_decimals_returns_18() {
    // 期望:WETH decimals = 18
    let client = Erc20Client::new(WETH_ADDR, provider());
    let d = client
        .decimals()
        .await
        .expect("WETH decimals should succeed");
    assert_eq!(d, 18);
}

#[tokio::test]
async fn erc20_client_weth_symbol_returns_weth() {
    // 期望:WETH symbol = "WETH"
    let client = Erc20Client::new(WETH_ADDR, provider());
    let s = client.symbol().await.expect("WETH symbol should succeed");
    assert_eq!(s, "WETH");
}

#[tokio::test]
async fn erc20_client_balance_of_vitalik_usdc() {
    // 期望:vitalik 地址持有 USDC(> 0)
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let client = Erc20Client::new(USDC_ADDR, provider());
    let balance = client
        .balance_of(VITALIK_ADDR)
        .await
        .expect("balance_of should succeed");
    // vitalik 在 mainnet 持有 USDC
    assert!(
        balance > alloy_primitives::U256::ZERO,
        "vitalik USDC balance should be > 0, got {}",
        balance
    );
}

#[tokio::test]
async fn erc20_client_info_caches_decimals_and_symbol() {
    // 期望:未知 token 的 info() 返回 None(未预填)
    let unknown = "0x1234567890123456789012345678901234567890";
    let client = Erc20Client::new(unknown, provider());
    let info = client.info();
    assert_eq!(info.address, unknown.to_lowercase());
    assert!(info.decimals.is_none());
    assert!(info.symbol.is_none());
}

#[tokio::test]
async fn erc20_client_with_known_token_presets_decimals() {
    // 期望:TokenInfo::with_known_token() 可以预填 decimals (USDC=6, WETH=18)
    let info = TokenInfo::with_known_token(USDC_ADDR);
    assert_eq!(info.decimals, Some(6));
    assert_eq!(info.symbol.as_deref(), Some("USDC"));

    let info = TokenInfo::with_known_token(WETH_ADDR);
    assert_eq!(info.decimals, Some(18));
    assert_eq!(info.symbol.as_deref(), Some("WETH"));
}

#[test]
fn token_info_address_normalized_to_lowercase() {
    // 期望:TokenInfo 地址总是 lowercase,方便比较
    let info = TokenInfo::new("0xA0B86991C6218B36C1D19D4A2E9EB0CE3606EB48");
    assert_eq!(info.address, "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
}

#[test]
fn token_info_unknown_token_returns_no_preset() {
    // 期望:未知 token 不预填
    let info = TokenInfo::new("0x1234567890123456789012345678901234567890");
    assert!(info.decimals.is_none());
    assert!(info.symbol.is_none());
}
