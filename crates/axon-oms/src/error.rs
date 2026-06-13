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
}
