use rust_decimal::Decimal;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExchangeError {
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("websocket disconnected: {reason}")]
    WebSocketDisconnected { reason: String },

    #[error("authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("order rejected: {reason}")]
    OrderRejected { reason: String },

    #[error("insufficient balance: required {required}, available {available}")]
    InsufficientBalance {
        required: Decimal,
        available: Decimal,
    },

    #[error("rate limited: wait {wait_ms}ms")]
    RateLimited { wait_ms: u64 },

    #[error("order not found: {0}")]
    OrderNotFound(String),

    #[error("parse error: {0}")]
    ParseError(String),

    #[error("api error: code={code}, msg={message}")]
    ApiError { code: i32, message: String },

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("websocket error: {0}")]
    WebSocket(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("circuit breaker open")]
    CircuitBreakerOpen,
}
