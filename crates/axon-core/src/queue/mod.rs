//! 事件队列：回测引擎的时间调度核心
//!
//! 按时间戳严格排序事件，支持批量加载、暂停/恢复/单步、回放等回测特性。
//!
//! TDD 规范：[`axon-design/01-tdd/01-phase1-core/07-event-queue.md`](../../../../axon-design/01-tdd/01-phase1-core/07-event-queue.md)
//!
//! # 模块
//!
//! - [`queued_event`]：队列条目（`QueuedEvent`） + 排序实现
//! - [`mode`]：运行模式（`QueueMode`）
//! - [`stats`]：统计信息（`QueueStats`）
//! - [`error`]：错误类型（`EventQueueError`）
//! - [`event_queue`]：主结构（`EventQueue`）

pub mod error;
pub mod event_queue;
pub mod mode;
pub mod queued_event;
pub mod stats;

pub use error::{EventQueueError, EventQueueResult};
pub use event_queue::EventQueue;
pub use mode::QueueMode;
pub use queued_event::QueuedEvent;
pub use stats::QueueStats;
