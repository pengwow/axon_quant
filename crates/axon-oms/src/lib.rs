//! # axon-oms
//!
//! 订单管理系统（OMS）：管理订单从创建到完成的完整生命周期。
//!
//! ## 核心功能
//!
//! - **订单状态机**：New → Submitted → Acknowledged → PartiallyFilled → Filled/Cancelled/Rejected
//! - **幂等性**：通过 idempotency_key 防止重复提交
//! - **快照与恢复**：支持崩溃后从快照恢复未完成订单
//! - **批量操作**：支持批量下单、撤单
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! use axon_oms::{OrderManager, Order, OrderStatus, Side, OrderType};
//! use rust_decimal::Decimal;
//!
//! // 创建 OMS
//! let oms = OrderManager::new();
//!
//! // 提交订单
//! let order = Order::new(
//!     "BTC-USDT".into(),
//!     Side::Buy,
//!     OrderType::Limit,
//!     Decimal::new(1, 3), // 0.001
//!     Decimal::from(50000),
//! );
//! let id = oms.submit(order).unwrap();
//!
//! // 更新状态
//! oms.update_status(id, OrderStatus::Acknowledged).unwrap();
//! ```
//!
//! ## 订单状态机
//!
//! ```text
//! New → Submitted → Acknowledged → PartiallyFilled → Filled
//!                                      ↓
//!                                  Cancelled
//!
//! New → Rejected
//! Submitted → Rejected
//! ```
//!
//! ## 性能
//!
//! | 操作 | 延迟 |
//! |------|------|
//! | submit | 1.2µs |
//! | update_status | 82ns |
//! | snapshot | 4.9µs (100 订单) |

pub mod error;
pub mod manager;
pub mod portfolio;
pub mod types;

pub use error::OmsError;
pub use manager::OrderManager;
pub use portfolio::{Portfolio, PortfolioError, PortfolioSnapshot, Position};
// 0.6.0 新增:`OMS_SNAPSHOT_VERSION_CURRENT` 等常量需显式 re-export
// (在 `types::*` 之外),让 `axon_oms::OMS_SNAPSHOT_VERSION_CURRENT` 可用。
pub use types::*;
pub use types::{OMS_SNAPSHOT_VERSION_CURRENT, OMS_SNAPSHOT_VERSION_LEGACY};

// Stage 4:`axon-oms` Python 绑定(PyO3)。仅在 `python` feature 启用时编译。
// 完整子模块结构见 `crates/axon-oms/src/python/mod.rs`。
#[cfg(feature = "python")]
pub mod python;
