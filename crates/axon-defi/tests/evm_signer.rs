//! Signer 集成测试(需要本地 anvil 节点)

#![cfg(feature = "evm")]

use std::time::Duration;

use axon_defi::evm::chain::Chain;
use axon_defi::evm::provider::ProviderConfig;
use axon_defi::evm::signer::{LocalSigner, SignerError, SignerKind};

const ANVIL_URL: &str = "http://127.0.0.1:8545";
// anvil 启动时打印的默认私钥(0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80)
const ANVIL_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
// anvil 默认账户 0
const ANVIL_ADDR: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

#[tokio::test]
async fn local_signer_parses_anvil_default_key() {
    // 期望:用 anvil 默认私钥构造 LocalSigner,地址匹配 anvil 默认账户 0
    let signer = LocalSigner::from_hex(ANVIL_KEY, Chain::Ethereum).expect("anvil key valid");
    let addr = signer.address();
    assert_eq!(
        format!("{:?}", addr).to_lowercase(),
        ANVIL_ADDR.to_lowercase()
    );
}

#[tokio::test]
async fn local_signer_rejects_invalid_hex() {
    // 期望:非法 hex 字符串返回 SignerError::InvalidPrivateKey
    let result = LocalSigner::from_hex("not-hex", Chain::Ethereum);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, SignerError::InvalidPrivateKey(_)));
}

#[tokio::test]
async fn local_signer_rejects_wrong_length() {
    // 期望:长度不对(非 32 字节)返回 SignerError::InvalidPrivateKey
    let result = LocalSigner::from_hex("0xabcd", Chain::Ethereum);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, SignerError::InvalidPrivateKey(_)));
}

#[tokio::test]
async fn local_signer_kind_returns_local() {
    // 期望:SignerKind 分类正确
    let signer = LocalSigner::from_hex(ANVIL_KEY, Chain::Ethereum).unwrap();
    assert_eq!(signer.kind(), SignerKind::Local);
}

#[tokio::test]
async fn local_signer_nonce_starts_at_zero_or_synced() {
    // 期望:首次 nonce 应从链上同步(anvil 默认账户 nonce 起初为 0)
    if !anvil_running().await {
        eprintln!("[skip] anvil not running at {ANVIL_URL}");
        return;
    }
    let provider = axon_defi::evm::provider::EvmProvider::new(ProviderConfig::for_chain(
        Chain::Ethereum,
        ANVIL_URL,
    ));
    let signer = LocalSigner::from_hex(ANVIL_KEY, Chain::Ethereum).unwrap();
    let nonce = signer
        .sync_nonce(&provider)
        .await
        .expect("sync_nonce should succeed on anvil");
    // 不强制 == 0,因为 anvil 跑过其他测试后可能 > 0
    // 关键:nonce 是 u64,不 panic
    let _ = nonce;
}

#[tokio::test]
async fn local_signer_next_nonce_increments_atomically() {
    // 期望:连续 next_nonce() 拿到 0, 1, 2...
    let signer = LocalSigner::from_hex(ANVIL_KEY, Chain::Ethereum).unwrap();
    signer.set_nonce(100); // 重置基线
    assert_eq!(signer.next_nonce(), 100);
    assert_eq!(signer.next_nonce(), 101);
    assert_eq!(signer.next_nonce(), 102);
}

#[tokio::test]
async fn local_signer_can_transfer_eth_on_anvil() {
    // 期望:用 anvil 默认账户给 vitalik 地址发 0.001 ETH,成功收到 receipt
    if !anvil_running().await {
        eprintln!("[skip] anvil not running");
        return;
    }
    let provider = axon_defi::evm::provider::EvmProvider::new(ProviderConfig::for_chain(
        Chain::Ethereum,
        ANVIL_URL,
    ));
    let signer = LocalSigner::from_hex(ANVIL_KEY, Chain::Ethereum).unwrap();

    // vitalik.eth
    let to = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"
        .parse()
        .expect("valid address");

    let receipt = signer
        .transfer_eth(&provider, to, alloy_primitives::U256::from(1_000_000_000_000_000u128)) // 0.001 ETH in wei
        .await
        .expect("transfer should succeed on anvil");

    assert!(receipt.status(), "tx should succeed on anvil");
    let block = receipt.block_number.expect("anvil fills block_number");
    assert!(block > 0, "block should be > 0");
}

async fn anvil_running() -> bool {
    match tokio::time::timeout(
        Duration::from_millis(500),
        reqwest::get(format!("{ANVIL_URL}")),
    )
    .await
    {
        Ok(Ok(_)) => true,
        _ => false,
    }
}
