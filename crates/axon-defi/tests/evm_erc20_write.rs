//! Erc20Client 写路径 + 事件解析测试
//!
//! 0.3.0 P0 Batch 2 / T1.6:
//! - 纯单测:approve_tx / transfer_tx 构造 + 事件日志解析
//! - 集成测试(anvil fork):approve + transfer 全流程 + receipt.status == 1

#![cfg(feature = "evm")]

use std::time::Duration;

use alloy_primitives::{Address, U256};

use axon_defi::evm::chain::Chain;
use axon_defi::evm::erc20::Erc20Client;
use axon_defi::evm::provider::{EvmProvider, ProviderConfig};
use axon_defi::evm::signer::LocalSigner;

const ANVIL_URL: &str = "http://127.0.0.1:8545";

// anvil 第 0 个账户的私钥(well-known,公开)
// 仅用于测试,生产绝不能用
const ANVIL_PRIVATE_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

// anvil 账户 1(随便写一个测试地址)
const RECIPIENT: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";

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

// ---------- 纯单测(不连真链) ----------

#[test]
fn approve_tx_constructs_with_selector_and_inputs() {
    let client = Erc20Client::new(USDC, provider());
    let spender = Address::parse_checksummed(RECIPIENT, None).unwrap();
    let amount = U256::from(1_000_000u64); // 1 USDC
    let tx = client.approve_tx(spender, amount).expect("approve_tx ok");
    // 验证 tx.to 是 Call(Address)
    assert!(matches!(tx.to, Some(alloy::primitives::TxKind::Call(_))));
    let input = tx.input.into_input().unwrap();
    // approve selector = 0x095ea7b3
    assert_eq!(&input[..4], &[0x09, 0x5e, 0xa7, 0xb3]);
    // 后跟 spender(address,32 字节) + amount(uint256,32 字节)
    assert_eq!(input.len(), 4 + 32 + 32);
}

#[test]
fn transfer_tx_constructs_with_selector_and_inputs() {
    let client = Erc20Client::new(USDC, provider());
    let to = Address::parse_checksummed(RECIPIENT, None).unwrap();
    let amount = U256::from(2_500_000u64);
    let tx = client.transfer_tx(to, amount).expect("transfer_tx ok");
    let input = tx.input.into_input().unwrap();
    // transfer selector = 0xa9059cbb
    assert_eq!(&input[..4], &[0xa9, 0x05, 0x9c, 0xbb]);
    assert_eq!(input.len(), 4 + 32 + 32);
}

#[test]
fn parse_approval_log_with_correct_signature() {
    // 用 alloy IERC20 实际生成的 signature,确保 roundtrip 解码正确
    use alloy::primitives::B256;

    let from = Address::from([0x33u8; 20]);
    let spender = Address::from([0x44u8; 20]);
    let value = U256::from(999_000u64);

    // IERC20::Approval event signature =
    // keccak256("Approval(address,address,uint256)") = 0x8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925
    let sig_bytes = alloy::primitives::hex::decode(
        "8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925",
    )
    .unwrap();
    let mut sig = [0u8; 32];
    sig.copy_from_slice(&sig_bytes);

    let mut t_owner = [0u8; 32];
    t_owner[12..].copy_from_slice(from.as_slice());
    let mut t_spender = [0u8; 32];
    t_spender[12..].copy_from_slice(spender.as_slice());
    let topics = vec![B256::from(sig), B256::from(t_owner), B256::from(t_spender)];

    let mut data = [0u8; 32];
    let be_bytes = value.to_be_bytes::<32>();
    data.copy_from_slice(&be_bytes);

    let (parsed_owner, parsed_spender, parsed_value) =
        Erc20Client::parse_approval_log(&topics, &data).expect("approval decode ok");
    assert_eq!(parsed_owner, from);
    assert_eq!(parsed_spender, spender);
    assert_eq!(parsed_value, value);
}

#[test]
fn parse_transfer_log_with_correct_signature() {
    use alloy::primitives::B256;

    let from = Address::from([0x55u8; 20]);
    let to = Address::from([0x66u8; 20]);
    let value = U256::from(7_777u64);

    // IERC20::Transfer event signature =
    // keccak256("Transfer(address,address,uint256)") = 0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef
    let sig_bytes = alloy::primitives::hex::decode(
        "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
    )
    .unwrap();
    let mut sig = [0u8; 32];
    sig.copy_from_slice(&sig_bytes);

    let mut t_from = [0u8; 32];
    t_from[12..].copy_from_slice(from.as_slice());
    let mut t_to = [0u8; 32];
    t_to[12..].copy_from_slice(to.as_slice());
    let topics = vec![B256::from(sig), B256::from(t_from), B256::from(t_to)];

    let mut data = [0u8; 32];
    let be_bytes = value.to_be_bytes::<32>();
    data.copy_from_slice(&be_bytes);

    let (parsed_from, parsed_to, parsed_value) =
        Erc20Client::parse_transfer_log(&topics, &data).expect("transfer decode ok");
    assert_eq!(parsed_from, from);
    assert_eq!(parsed_to, to);
    assert_eq!(parsed_value, value);
}

#[test]
fn parse_transfer_log_rejects_wrong_signature() {
    use alloy::primitives::B256;

    let topics = vec![B256::from([0xffu8; 32]), B256::ZERO, B256::ZERO];
    let data = [0u8; 32];
    let res = Erc20Client::parse_transfer_log(&topics, &data);
    assert!(res.is_err());
}

// ---------- anvil fork 集成测试(无 anvil 自动 skip) ----------

#[tokio::test]
async fn approve_with_local_signer_succeeds_on_anvil() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let signer = test_signer();
    let client = Erc20Client::new(USDC, provider());
    let spender = Address::parse_checksummed(RECIPIENT, None).unwrap();
    let amount = U256::from(1_000_000u64);

    let receipt = client
        .approve(&signer, &provider(), spender, amount)
        .await
        .expect("approve ok");
    assert!(receipt.status(), "approve tx should succeed");
    assert!(!receipt.transaction_hash.is_zero());
}

#[tokio::test]
async fn transfer_with_local_signer_succeeds_on_anvil() {
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let signer = test_signer();
    let client = Erc20Client::new(USDC, provider());
    let to = Address::parse_checksummed(RECIPIENT, None).unwrap();
    // 1 USDC = 1_000_000 (USDC decimals=6)
    let amount = U256::from(1_000_000u64);

    let receipt = client
        .transfer(&signer, &provider(), to, amount)
        .await
        .expect("transfer ok");
    assert!(receipt.status(), "transfer tx should succeed");
    assert!(!receipt.transaction_hash.is_zero());
    // receipt 应该至少有 1 个 Transfer 日志
    assert!(!receipt.inner.logs().is_empty());
}
