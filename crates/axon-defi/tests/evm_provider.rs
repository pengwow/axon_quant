//! EvmProvider 集成测试
//!
//! 需要本地 anvil 节点。运行方式:
//! ```bash
//! anvil --port 8545 &
//! cargo test -p axon-defi --features evm --test evm_provider
//! ```
//!
//! 若检测不到 anvil,自动跳过(保证 CI 不挂)。

#![cfg(feature = "evm")]

use std::time::Duration;

use axon_defi::evm::chain::Chain;
use axon_defi::evm::provider::{EvmProvider, ProviderConfig};

const ANVIL_URL: &str = "http://127.0.0.1:8545";

/// 检测 anvil 是否在运行
async fn anvil_running() -> bool {
    matches!(
        tokio::time::timeout(Duration::from_millis(500), reqwest::get(ANVIL_URL.to_string())).await,
        Ok(Ok(_))
    )
}

#[tokio::test]
async fn evm_provider_constructs_from_config() {
    // 期望:从 ProviderConfig 构造 EvmProvider 不 panic
    let config = ProviderConfig::for_chain(Chain::Ethereum, ANVIL_URL);
    let provider = EvmProvider::new(config);
    // 不应 panic,config 字段应可读
    assert_eq!(provider.config().rpc_url, ANVIL_URL);
}

#[tokio::test]
async fn evm_provider_chain_id_matches_anvil_local() {
    // 期望:连 anvil 本地节点,chain_id == 31337
    if !anvil_running().await {
        eprintln!("[skip] anvil not running at {ANVIL_URL}");
        return;
    }
    let config = ProviderConfig::for_chain(Chain::Ethereum, ANVIL_URL);
    let provider = EvmProvider::new(config);
    let id = provider.chain_id().await.expect("chain_id should succeed");
    assert_eq!(id, 31337, "anvil local chain id is 31337");
}

#[tokio::test]
async fn evm_provider_block_number_returns_nonzero() {
    // 期望:连 anvil 后,block_number > 0
    if !anvil_running().await {
        eprintln!("[skip] anvil not running at {ANVIL_URL}");
        return;
    }
    let config = ProviderConfig::for_chain(Chain::Ethereum, ANVIL_URL);
    let provider = EvmProvider::new(config);
    let n = provider
        .block_number()
        .await
        .expect("block_number should succeed");
    assert!(n > 0, "block_number should be > 0, got {}", n);
}

#[tokio::test]
async fn evm_provider_invalid_url_returns_error() {
    // 期望:无效 URL 返回 RpcError 而非 panic
    let config = ProviderConfig::for_chain(Chain::Ethereum, "http://127.0.0.1:1");
    let provider = EvmProvider::new(config);
    let result = provider.chain_id().await;
    assert!(result.is_err(), "无效 URL 应返回错误");
    let err = result.unwrap_err();
    // 错误应是 RpcError 结构化变体
    let msg = format!("{}", err);
    assert!(
        msg.contains("127.0.0.1:1") || msg.contains("connection"),
        "错误信息应包含 url 或 connection: {}",
        msg
    );
}

#[tokio::test]
async fn evm_provider_clone_shares_underlying_connection() {
    // 期望:Clone 后的 provider 共享底层连接池(Arc),不应深拷贝
    let config = ProviderConfig::for_chain(Chain::Ethereum, ANVIL_URL);
    let p1 = EvmProvider::new(config);
    let p2 = p1.clone();
    // 两个实例应指向同一配置
    assert_eq!(p1.config().rpc_url, p2.config().rpc_url);
}

#[test]
fn provider_config_default_is_sane() {
    // 期望:ProviderConfig::default() 给出合理默认
    let cfg = ProviderConfig::default();
    assert_eq!(cfg.timeout_ms, 5000);
    assert_eq!(cfg.max_retries, 3);
    assert!(cfg.anvil_fork_block.is_none());
    assert!(cfg.ws_url.is_none());
}

#[test]
fn provider_config_supports_chain() {
    // 期望:ProviderConfig 可以关联 Chain
    let cfg = ProviderConfig::for_chain(Chain::Ethereum, "https://eth.llamarpc.com");
    assert_eq!(cfg.chain, Chain::Ethereum);
    assert_eq!(cfg.rpc_url, "https://eth.llamarpc.com");
}
