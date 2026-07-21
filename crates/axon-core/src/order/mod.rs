//! 订单类型系统
//!
//! 支持市价单、限价单、止损单、止损限价单、冰山单等多种订单类型，
//! 配套订单有效期（TIF）、订单状态机、拒绝原因等完整语义。
//!
//! TDD 规范：[`axon-design/01-tdd/01-phase1-core/03-order-types.md`](../../../../axon-design/01-tdd/01-phase1-core/03-order-types.md)
//!
//! # 模块组织
//!
//! - [`types`]：订单类型枚举（`Market` / `Limit` / `Stop` / `StopLimit` / `Iceberg`）
//! - [`tif`]：订单有效期枚举（`GTC` / `IOC` / `FOK` / `GFD` / `FAK`）
//! - [`status`]：订单状态机（7 态）与拒绝原因（8 种）
//! - [`core`]：[`Order`] 主体结构与生命周期方法
//! - [`error`]：错误类型
//! - [`fill`]：per-fill 元数据 + 状态机(0.8.0 Phase 3.2 A1.1)

pub mod core;
pub mod error;
pub mod fill;
pub mod status;
pub mod tif;
pub mod types;

pub use core::Order;
pub use error::{OrderError, OrderResult};
pub use fill::{FillRecord, FillState};
pub use status::{OrderStatus, RejectReason};
pub use tif::TimeInForce;
pub use types::OrderType;

/// 订单 ID 类型别名（u64 序列）
///
/// 后续撮合引擎负责 ID 的分配与全局唯一性。
pub type OrderId = u64;
