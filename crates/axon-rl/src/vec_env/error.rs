//! 向量化环境错误类型

use thiserror::Error;

use crate::env::error::EnvError;

/// 向量化环境相关错误
#[derive(Error, Debug, Clone, PartialEq)]
pub enum VecEnvError {
    /// 通道发送失败
    #[error("channel send failed: {0}")]
    ChannelSend(String),

    /// 通道接收失败
    #[error("channel receive failed: {0}")]
    ChannelRecv(String),

    /// 工作线程 panic
    #[error("worker thread for env {0} panicked")]
    WorkerPanic(usize),

    /// 单个环境错误
    #[error("env {0} error: {1}")]
    Env(usize, String),

    /// 动作 / 种子的数量与 `num_envs` 不匹配
    #[error("dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// 期望长度
        expected: usize,
        /// 实际长度
        got: usize,
    },

    /// 所有环境都失败
    #[error("all environments failed")]
    AllFailed,

    /// 环境数量为 0
    #[error("num_envs must be > 0, got 0")]
    ZeroEnvs,
}

impl VecEnvError {
    /// 失败的环境索引（若有）
    pub fn env_index(&self) -> Option<usize> {
        match self {
            Self::WorkerPanic(i) | Self::Env(i, _) => Some(*i),
            _ => None,
        }
    }
}

/// `EnvError` → `VecEnvError::Env(usize, _)` 的自动转换
///
/// 供 `vec_env` 内部使用：单个环境的 `reset` / `step` 失败时，
/// 通过 `?` 自动包装为 `VecEnvError::Env(0, ...)`。
impl From<EnvError> for VecEnvError {
    fn from(err: EnvError) -> Self {
        Self::Env(0, err.to_string())
    }
}

/// 向量化环境结果别名
pub type VecEnvResult<T> = Result<T, VecEnvError>;
