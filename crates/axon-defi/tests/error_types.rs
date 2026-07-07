//! 错误类型测试

use axon_defi::error::DefiError;

#[test]
fn chain_error_carries_chain_id() {
    // 期望:ChainError 变体携带 chain_id + 错误源
    let err = DefiError::ChainError {
        chain_id: 1,
        reason: "RPC down".to_string(),
    };
    let msg = format!("{}", err);
    assert!(msg.contains("chain 1"), "msg 应包含 chain id: {}", msg);
    assert!(msg.contains("RPC down"), "msg 应包含 reason: {}", msg);
}

#[test]
fn rpc_error_carries_url_and_body() {
    // 期望:RpcError 变体携带 url + status + body
    let err = DefiError::RpcError {
        url: "https://eth.llamarpc.com".to_string(),
        status: 429,
        body: "rate limit".to_string(),
    };
    let msg = format!("{}", err);
    assert!(msg.contains("eth.llamarpc.com"), "msg 应包含 url: {}", msg);
    assert!(msg.contains("429"), "msg 应包含 status: {}", msg);
    assert!(msg.contains("rate limit"), "msg 应包含 body: {}", msg);
}

#[test]
fn contract_error_carries_address_and_method() {
    // 期望:ContractError 变体携带 address + method + reason
    let err = DefiError::ContractError {
        address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
        method: "balanceOf".to_string(),
        reason: "execution reverted".to_string(),
    };
    let msg = format!("{}", err);
    assert!(msg.contains("balanceOf"), "msg 应包含 method: {}", msg);
    assert!(
        msg.contains("execution reverted"),
        "msg 应包含 reason: {}",
        msg
    );
}

#[test]
fn legacy_variants_still_compile() {
    // 兼容性:既有变体不能破坏
    let _ = DefiError::UnsupportedChain(999);
    let _ = DefiError::RpcErrorLegacy("test".to_string());
    let _ = DefiError::TransactionFailed("test".to_string());
    let _ = DefiError::NoRouteFound;
    let _ = DefiError::ConfigError("test".to_string());
}
