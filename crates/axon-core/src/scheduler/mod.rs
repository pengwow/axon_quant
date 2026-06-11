//! 调度器：回测时钟与定时任务管理
//!
//! 调度器驱动回测时钟推进，按时间顺序触发定时任务与周期任务。
//!
//! TDD 规范：[`axon-design/01-tdd/01-phase1-core/08-scheduler.md`](../../../../axon-design/01-tdd/01-phase1-core/08-scheduler.md)
//!
//! # 模块组织
//!
//! - [`clock`]：模拟时钟（[`SimulatedClock`]）
//! - [`task`]：任务定义（[`Task`] / [`TaskId`] / [`TaskStatus`] / [`RepeatPolicy`]）
//! - [`callback`]：任务回调接口（[`TaskCallback`]）
//! - [`context`]：任务上下文（[`SchedulerContext`]）
//! - [`error`]：错误类型（[`SchedulerError`]）
//! - [`core`]：调度器主结构（[`Scheduler`]）

pub mod callback;
pub mod clock;
pub mod context;
pub mod core;
pub mod error;
pub mod task;

pub use callback::{ClosureCallback, TaskCallback};
pub use clock::SimulatedClock;
pub use context::SchedulerContext;
pub use core::{Scheduler, SchedulerStats};
pub use error::{SchedulerError, SchedulerResult};
pub use task::{RepeatPolicy, Task, TaskId, TaskStatus};
