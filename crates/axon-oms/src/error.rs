use thiserror::Error;

#[derive(Debug, Error)]
pub enum OmsError {
    #[error("order not found: {0}")]
    OrderNotFound(String),

    #[error("invalid state transition: {from} -> {to}")]
    InvalidTransition { from: String, to: String },

    #[error("duplicate idempotency key: {0}")]
    DuplicateIdempotencyKey(String),

    #[error("order already in terminal state: {0}")]
    AlreadyTerminal(String),

    #[error("exchange rejected: {0}")]
    ExchangeRejected(String),

    #[error("network error: {0}")]
    NetworkError(String),

    #[error("serialization error: {0}")]
    SerializationError(String),

    #[error("recovery failed: {0}")]
    RecoveryFailed(String),

    /// Stage B-MVP 新增 — Portfolio 错误
    ///
    /// **为何不把 PortfolioError 变体逐个嵌入**:避免 OmsError 随 portfolio
    /// 复杂度膨胀;OmsError 与 PortfolioError 1:1 映射 + 字符串化,Stage B-TradingBridge
    /// 时在 axon-llm 侧做精细分类。
    #[error("portfolio error: {0}")]
    Portfolio(String),
}
