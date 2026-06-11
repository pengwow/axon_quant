//! HPO 统一错误类型

use thiserror::Error;

/// HPO 错误类型
#[derive(Debug, Error)]
pub enum HPOError {
    /// 配置错误
    #[error("config error: {0}")]
    Config(String),

    /// 搜索空间错误
    #[error("search space error: {0}")]
    SearchSpace(String),

    /// trial 执行错误
    #[error("trial {trial_id} failed: {message}")]
    TrialFailed {
        /// trial ID
        trial_id: i32,
        /// 错误信息
        message: String,
    },

    /// Optuna 错误（Python 侧）
    #[error("optuna error: {0}")]
    Optuna(String),

    /// 多目标方向不匹配
    #[error("directions length mismatch: expected {expected}, got {got}")]
    DirectionsMismatch {
        /// 期望长度
        expected: usize,
        /// 实际长度
        got: usize,
    },

    /// trial 结果缺失
    #[error("no values for trial {0}")]
    MissingValues(i32),

    /// IO 错误
    #[error("io error: {0}")]
    Io(String),

    /// 序列化错误
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// HPO Result 类型别名
pub type HPOResult<T> = Result<T, HPOError>;
