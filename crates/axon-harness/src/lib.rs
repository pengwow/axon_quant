//! Harness 编排系统 trait 接口
//!
//! 定义 HarnessPolicy / ToolGate / BudgetGuard 等核心 trait，
//! 以及 HarnessBridge 桥接器。

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod bridge;
pub mod policy;
pub mod types;

pub use bridge::HarnessBridge;
pub use policy::{BudgetGuard, HarnessPolicy, ToolGate};
pub use types::*;
