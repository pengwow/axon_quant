//! 事件系统
//!
//! 事件驱动架构是 AXON 的核心。所有状态变化都通过事件传播，
//! 确保回测可重现、系统可观测。
//!
//! # 模块
//!
//! - [`types`]：统一事件枚举 + 类型分类位掩码
//! - [`market`]：市场数据事件
//! - [`order`]：订单事件
//! - [`fill`]：成交事件
//! - [`mark`]：标记价格事件
//! - [`system`]：系统事件
//! - [`handler`]：事件处理器 trait
//! - [`builder`]：事件构建器（自增序列号）
//! - [`router`]：事件路由器 + 收集器
//! - [`error`]：事件模块错误
//!
//! TDD 规范：[`axon-design/01-tdd/01-phase1-core/04-events.md`](../../../../axon-design/01-tdd/01-phase1-core/04-events.md)

pub mod builder;
pub mod error;
pub mod fill;
pub mod funding;
pub mod handler;
pub mod mark;
pub mod market;
pub mod order;
pub mod router;
pub mod system;
pub mod types;

pub use builder::EventBuilder;
pub use error::{EventError, EventResult};
pub use fill::FillEvent;
pub use funding::{FundingEvent, FundingSchedule};
pub use handler::EventHandler;
pub use mark::MarkEvent;
pub use market::{MarketDataEvent, MarketDataPayload};
pub use order::{OrderAction, OrderEvent};
pub use router::{EventCollector, EventRouter};
pub use system::{SystemAction, SystemEvent};
pub use types::{Event, EventType};
