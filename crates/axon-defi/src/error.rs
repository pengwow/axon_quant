//! DeFi 错误类型

use thiserror::Error;

/// DeFi 错误
#[derive(Debug, Error)]
pub enum DefiError {
    /// 不支持的链
    #[error("unsupported chain: {0}")]
    UnsupportedChain(u64),

    /// RPC 错误
    #[error("RPC error: {0}")]
    RpcError(String),

    /// 交易失败
    #[error("transaction failed: {0}")]
    TransactionFailed(String),

    /// 路由未找到
    #[error("no route found")]
    NoRouteFound,

    /// 滑点过大
    #[error("slippage too high: actual {actual}%, max {max}%")]
    SlippageTooHigh {
        /// 实际滑点
        actual: f64,
        /// 最大滑点
        max: f64,
    },

    /// 风控拒绝
    #[error("risk rejected: {0}")]
    RiskRejected(String),

    /// 跨链错误
    #[error("bridge error: {0}")]
    BridgeError(String),

    /// 合约错误
    #[error("contract error: {0}")]
    ContractError(String),

    /// 配置错误
    #[error("config error: {0}")]
    ConfigError(String),
}
