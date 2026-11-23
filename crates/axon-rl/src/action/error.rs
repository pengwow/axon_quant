//! 动作空间错误类型

use thiserror::Error;

use crate::action::state::PortfolioState;
use crate::action::types::{Action, ActionSpace};

/// 动作空间错误
#[derive(Error, Debug, Clone, PartialEq)]
pub enum ActionError {
    /// 离散动作 index 越界
    #[error("Invalid action index {index} for space of size {size}")]
    InvalidIndex {
        /// 提供的 index
        index: usize,
        /// 空间大小
        size: usize,
    },

    /// 动作被 mask 阻止
    #[error("Action masked: {reason}")]
    Masked {
        /// 阻止原因
        reason: String,
    },

    /// 连续动作值越界
    #[error("Continuous action value {value} out of range [{min}, {max}]")]
    OutOfRange {
        /// 实际值
        value: f64,
        /// 下界
        min: f64,
        /// 上界
        max: f64,
    },

    /// 无持仓可卖
    #[error("No position to sell: current position = {position}")]
    NoPositionToSell {
        /// 当前持仓
        position: f64,
    },

    /// 保证金不足
    #[error("Insufficient margin: requested {requested}, available {available}")]
    InsufficientMargin {
        /// 请求金额
        requested: f64,
        /// 可用金额
        available: f64,
    },

    /// 动作向量长度不匹配
    #[error("Action vector length mismatch: expected {expected}, got {actual}")]
    VectorLengthMismatch {
        /// 期望长度
        expected: usize,
        /// 实际长度
        actual: usize,
    },
}

/// 动作空间统一 Result 类型
pub type ActionResult<T> = Result<T, ActionError>;

/// 验证动作合法性
pub fn validate_action(
    action: &Action,
    space: &ActionSpace,
    state: &PortfolioState,
) -> ActionResult<()> {
    match space {
        ActionSpace::Discrete(d) => {
            let index = match action.action_type {
                ActionSpaceKind::Discrete(i) => i,
                ActionSpaceKind::Continuous(_) => {
                    return Err(ActionError::OutOfRange {
                        value: 0.0,
                        min: 0.0,
                        max: d.n as f64,
                    });
                }
            };
            if index >= d.n {
                return Err(ActionError::InvalidIndex { index, size: d.n });
            }
            let mask = d.valid_mask(state);
            if !mask[index] {
                return Err(ActionError::Masked {
                    reason: format!("Action index {} is not valid for current state", index),
                });
            }
            Ok(())
        }
        ActionSpace::Continuous(c) => {
            for &v in &action.raw {
                if v < c.min || v > c.max {
                    return Err(ActionError::OutOfRange {
                        value: v,
                        min: c.min,
                        max: c.max,
                    });
                }
            }
            Ok(())
        }
    }
}

use crate::action::types::ActionSpaceKind;
