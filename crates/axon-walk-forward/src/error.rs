//! 统一错误类型

use thiserror::Error;

/// Walk-Forward 错误
#[derive(Debug, Error)]
pub enum WalkForwardError {
    /// 配置错误
    #[error("config error: {0}")]
    Config(String),

    /// 数据不足
    #[error("insufficient data: need {need}, got {got}")]
    InsufficientData {
        /// 所需样本数
        need: usize,
        /// 实际可用样本数
        got: usize,
    },

    /// 索引越界
    #[error("index out of bounds: {0}")]
    IndexOutOfBounds(String),

    /// 检测到数据泄漏
    #[error("leakage detected: {0}")]
    LeakageDetected(String),

    /// 序列化错误
    #[error("serialization error: {0}")]
    Serialization(String),

    /// IO 错误
    #[error("io error: {0}")]
    Io(String),
}

/// Walk-Forward Result 类型别名
pub type WalkForwardResult<T> = Result<T, WalkForwardError>;
