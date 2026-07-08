//! V3Router 集成测试(需要本地 anvil 节点)
//!
//! 0.3.0 P0 Batch 3 / T1.8:验证 SwapRouter02 真发交易

#![cfg(feature = "evm")]

use std::time::Duration;

use alloy_primitives::{Address, U256};

use axon_defi::dex::v3_router::{SwapParams, V3Router};
use axon_defi::evm::chain::Chain;
use axon_defi::evm::erc20::Erc20Client;
use axon_defi::evm::provider::{EvmProvider, ProviderConfig};
use axon_defi::evm::signer::LocalSigner;

const ANVIL_URL: &str = "http://127.0.0.1:8545";

const ANVIL_PRIVATE_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";

async fn anvil_running() -> bool {
    matches!(
        tokio::time::timeout(Duration::from_millis(500), reqwest::get(ANVIL_URL)).await,
        Ok(Ok(_))
    )
}

fn provider() -> EvmProvider {
    EvmProvider::new(ProviderConfig::for_chain(Chain::Ethereum, ANVIL_URL))
}

fn test_signer() -> LocalSigner {
    LocalSigner::from_hex(ANVIL_PRIVATE_KEY, Chain::Ethereum).expect("signer ok")
}

// ---------- 纯单测 ----------

#[test]
fn v3_router_constructs_with_default_address() {
    let r = V3Router::new(provider(), Chain::Ethereum);
    assert_eq!(
        r.address().to_lowercase(),
        "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45"
    );
    assert_eq!(r.chain(), Chain::Ethereum);
}

#[test]
fn v3_router_with_custom_address() {
    let r = V3Router::new(provider(), Chain::Polygon)
        .with_router_address("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");
    assert_eq!(r.address(), "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45");
}

#[test]
fn build_tx_constructs_with_correct_target() {
    let r = V3Router::new(provider(), Chain::Ethereum);
    let signer = test_signer();
    let usdc = Address::parse_checksummed(USDC, None).unwrap();
    let weth = Address::parse_checksummed(WETH, None).unwrap();
    let params = SwapParams::new(usdc, weth, 3000, U256::from(1_000_000u64))
        .with_min_out(U256::from(900_000u64));
    let tx = r.build_tx(&signer, params).expect("build_tx ok");
    // 验证目标是 router
    let to_addr = match tx.to {
        Some(alloy::primitives::TxKind::Call(a)) => a,
        _ => panic!("should target a contract"),
    };
    assert_eq!(
        to_addr,
        Address::parse_checksummed("0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45", None).unwrap()
    );
    // input 应是 exactInputSingle ABI 编码
    let input = tx.input.into_input().unwrap();
    // exactInputSingle 函数签名 7 个参数(address,address,uint24,address,uint256,uint256,uint160)
    // → selector(4) + 7 * 32 = 228 字节
    assert_eq!(input.len(), 4 + 7 * 32);
}

// ---------- anvil 集成测试 ----------

#[tokio::test]
async fn swap_usdc_to_weth_succeeds_on_anvil() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let r = V3Router::new(provider(), Chain::Ethereum);
    let signer = test_signer();
    let token_in = Erc20Client::new(USDC, provider());
    let usdc = Address::parse_checksummed(USDC, None).unwrap();
    let weth = Address::parse_checksummed(WETH, None).unwrap();
    let params = SwapParams::new(usdc, weth, 3000, U256::from(1_000_000u64));

    let receipt = r.swap(&signer, &token_in, params).await.expect("swap ok");
    assert!(receipt.status(), "swap tx should succeed");
    assert!(!receipt.transaction_hash.is_zero());
}
