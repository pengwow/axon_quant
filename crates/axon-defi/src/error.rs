//! DeFi 错误类型
//!
//! 0.3.0 P0 扩展:引入 3 个结构化变体以支持真链 RPC + 合约交互。
//! - [`DefiError::ChainError`]:链级错误,带 `chain_id` + `source`
//! - [`DefiError::RpcError`]:HTTP/RPC 调用错误,带 `url` + `status` + `body`
//! - [`DefiError::ContractError`]:合约调用错误,带 `address` + `method` + `reason`
//!
//! 旧 `RpcError(String)` 重命名为 `RpcErrorLegacy(String)` 以便调用方迁移;
//! 0.3.0 收口时全部调用方更新后可删除。

use thiserror::Error;

/// DeFi 错误
#[derive(Debug, Error)]
pub enum DefiError {
    /// 不支持的链
    #[error("unsupported chain: {0}")]
    UnsupportedChain(u64),

    /// RPC 错误(结构化,0.3.0 新增)
    #[error("RPC error at {url} (status {status}): {body}")]
    RpcError {
        /// 完整 URL
        url: String,
        /// HTTP 状态码(0 表示网络层错误)
        status: u16,
        /// 响应体(截断到 256 字符)
        body: String,
    },

    /// 旧 RPC 错误(字符串,0.3.0 重命名,后续删除)
    #[error("RPC error (legacy): {0}")]
    RpcErrorLegacy(String),

    /// 链级错误(0.3.0 新增)
    #[error("chain {chain_id} error: {reason}")]
    ChainError {
        /// 链 ID
        chain_id: u64,
        /// 错误原因(非 std error source,故用 reason 命名避免 thiserror 误判)
        reason: String,
    },

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

    /// 合约错误(0.3.0 改为结构化)
    #[error("contract {address}.{method} error: {reason}")]
    ContractError {
        /// 合约地址
        address: String,
        /// 方法名
        method: String,
        /// 失败原因
        reason: String,
    },

    /// 旧合约错误(字符串,0.3.0 重命名,后续删除)
    #[error("contract error (legacy): {0}")]
    ContractErrorLegacy(String),

    /// 配置错误
    #[error("config error: {0}")]
    ConfigError(String),
}

impl DefiError {
    /// 截断 body 字符串到 256 字符
    ///
    /// 防止 RPC 错误响应体过大时污染日志
    pub fn truncated_body(body: &str) -> String {
        const MAX: usize = 256;
        if body.len() <= MAX {
            body.to_string()
        } else {
            let mut s = body[..MAX].to_string();
            s.push_str("...<truncated>");
            s
        }
    }
}
