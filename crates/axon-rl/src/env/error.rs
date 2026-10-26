//! 交易环境错误类型

use thiserror::Error;

/// 交易环境相关错误
#[derive(Error, Debug, Clone, PartialEq)]
pub enum EnvError {
    /// episode 已结束，不能继续 step
    #[error("episode already done at step {0}")]
    EpisodeAlreadyDone(usize),

    /// 非法动作
    #[error("invalid action: {0}")]
    InvalidAction(String),

    /// 数据耗尽
    #[error("data exhausted at step {0}, max steps: {1}")]
    DataExhausted(usize, usize),

    /// 数据为空
    #[error("market data is empty")]
    EmptyMarketData,

    /// 行情历史不足观测窗口（初始化时校验）
    #[error("insufficient history: need {needed} bars, have {have}")]
    InsufficientHistory {
        /// 观测窗口完整所需的根数
        needed: usize,
        /// 实际提供的根数
        have: usize,
    },

    /// 观测计算失败
    #[error("observation computation failed: {0}")]
    ObservationError(String),

    /// 奖励计算失败
    #[error("reward computation failed: {0}")]
    RewardError(String),

    /// 动作转换失败
    #[error("action conversion failed: {0}")]
    ActionError(String),
}

impl EnvError {
    /// 转换为 PyO3 Python 异常
    #[cfg(feature = "python")]
    pub fn to_py_err(self) -> pyo3::PyErr {
        use pyo3::exceptions::{PyStopIteration, PyValueError};
        match self {
            EnvError::EpisodeAlreadyDone(_) | EnvError::DataExhausted(_, _) => {
                PyStopIteration::new_err(self.to_string())
            }
            EnvError::InvalidAction(_)
            | EnvError::EmptyMarketData
            | EnvError::InsufficientHistory { .. }
            | EnvError::ActionError(_)
            | EnvError::ObservationError(_)
            | EnvError::RewardError(_) => PyValueError::new_err(self.to_string()),
        }
    }

    /// 是否可重试
    ///
    /// - 终态错误（episode 已结束、数据耗尽）⇒ 不可重试
    /// - 业务错误（动作非法、计算失败）⇒ 不可重试
    ///
    /// 当前所有变体都是不可重试的逻辑错误。
    pub fn is_retryable(&self) -> bool {
        match self {
            EnvError::EpisodeAlreadyDone(_)
            | EnvError::InvalidAction(_)
            | EnvError::DataExhausted(_, _)
            | EnvError::EmptyMarketData
            | EnvError::InsufficientHistory { .. }
            | EnvError::ObservationError(_)
            | EnvError::RewardError(_)
            | EnvError::ActionError(_) => false,
        }
    }
}

/// 环境操作结果别名
pub type EnvResult<T> = Result<T, EnvError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_variants_not_retryable() {
        let cases = vec![
            EnvError::EpisodeAlreadyDone(5),
            EnvError::InvalidAction("bad action".into()),
            EnvError::DataExhausted(10, 10),
            EnvError::EmptyMarketData,
            EnvError::InsufficientHistory {
                needed: 30,
                have: 10,
            },
            EnvError::ObservationError("norm failed".into()),
            EnvError::RewardError("pnl calc".into()),
            EnvError::ActionError("invalid side".into()),
        ];
        for e in cases {
            assert!(!e.is_retryable(), "{e:?} should not be retryable");
            // 验证 Display 实现不 panic
            let _ = e.to_string();
        }
    }

    #[test]
    fn test_episode_already_done_display() {
        let e = EnvError::EpisodeAlreadyDone(42);
        assert!(e.to_string().contains("42"));
    }

    #[test]
    fn test_data_exhausted_display() {
        let e = EnvError::DataExhausted(100, 50);
        let msg = e.to_string();
        assert!(msg.contains("100"));
        assert!(msg.contains("50"));
    }
}
