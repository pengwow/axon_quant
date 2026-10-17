//! 统一策略 trait 与实现
//!
//! 设计原则：
//! - Strategy 不知道自己在回测还是实盘，只管"给我 bar window 的 state，我返回 Action"
//! - 回测 Runner 和实盘 EventDrivenLoop 消费 Strategy 的方式完全相同（逐 bar 调 decide）
//! - 内置实现：SdmStrategyImpl（快路径）、RuleStrategyImpl（纯规则）、SwarmRouteStrategyImpl（快慢路由）

pub mod impl_;
pub mod trait_;

pub use impl_::*;
pub use trait_::*;

#[cfg(feature = "python")]
pub mod python;
