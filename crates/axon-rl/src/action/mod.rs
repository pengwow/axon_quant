//! 动作空间模块
//!
//! 定义 RL 智能体的动作接口：
//! - 离散动作：`DiscreteActionSpace`（Hold + Buy + Sell + 量级分箱）
//! - 连续动作：`ContinuousActionSpace`（目标仓位比例 `[-1, 1]`）
//! - 动作转换器：把 RL 动作映射到具体订单
//! - 动作掩码：根据组合状态过滤非法动作
//! - 动作平滑器：EMA + delta 限制，防止过度交易
//!
//! # 子模块
//!
//! - [`types`]：核心类型（DiscreteAction / ContinuousAction / ActionSpace / Action）
//! - [`error`]：错误类型与统一 Result
//! - [`state`]：投资组合状态（动作推断的输入）
//! - [`converter`]：动作 → 订单转换器（离散 / 连续）
//! - [`smoother`]：动作平滑器（EMA + max delta）

#![deny(unsafe_code)]

pub mod converter;
pub mod error;
pub mod smoother;
pub mod state;
pub mod types;

#[cfg(test)]
mod tests;

pub use converter::{
    ActionConverter, ContinuousActionConverter, DiscreteActionConverter, OrderType,
};
pub use error::{ActionError, ActionResult, validate_action};
pub use smoother::ActionSmoother;
pub use state::PortfolioState;
pub use types::{
    Action, ActionSpace, ActionSpaceKind, ContinuousActionSpace, DiscreteAction,
    DiscreteActionSpace, QuantityBin, TradingDirection, apply_action_mask,
};
