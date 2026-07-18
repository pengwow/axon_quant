use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use axon_core::Instrument;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderId(pub Uuid);

impl OrderId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for OrderId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for OrderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderType {
    Limit,
    Market,
    StopLoss,
    StopLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    New,
    Submitted,
    Acknowledged,
    PartiallyFilled {
        filled_qty: Decimal,
        avg_price: Decimal,
    },
    Filled {
        filled_qty: Decimal,
        avg_price: Decimal,
    },
    Cancelled {
        filled_qty: Decimal,
    },
    Rejected {
        reason: String,
    },
}

impl OrderStatus {
    pub fn can_transition_to(&self, next: &OrderStatus) -> bool {
        matches!(
            (self, next),
            (OrderStatus::New, OrderStatus::Submitted)
                | (OrderStatus::New, OrderStatus::Rejected { .. })
                | (OrderStatus::Submitted, OrderStatus::Acknowledged)
                | (OrderStatus::Submitted, OrderStatus::Rejected { .. })
                | (
                    OrderStatus::Acknowledged,
                    OrderStatus::PartiallyFilled { .. }
                )
                | (OrderStatus::Acknowledged, OrderStatus::Filled { .. })
                | (OrderStatus::Acknowledged, OrderStatus::Cancelled { .. })
                | (
                    OrderStatus::PartiallyFilled { .. },
                    OrderStatus::PartiallyFilled { .. }
                )
                | (
                    OrderStatus::PartiallyFilled { .. },
                    OrderStatus::Filled { .. }
                )
                | (
                    OrderStatus::PartiallyFilled { .. },
                    OrderStatus::Cancelled { .. }
                )
                // Stage B-MVP 新增 — 状态机反向回滚(fill event 失败时 reverse 用)
                | (
                    OrderStatus::Filled { .. },
                    OrderStatus::Acknowledged
                )
                | (
                    OrderStatus::PartiallyFilled { .. },
                    OrderStatus::Acknowledged
                )
        )
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OrderStatus::Filled { .. }
                | OrderStatus::Cancelled { .. }
                | OrderStatus::Rejected { .. }
        )
    }

    pub fn filled_quantity(&self) -> Decimal {
        match self {
            OrderStatus::PartiallyFilled { filled_qty, .. } => *filled_qty,
            OrderStatus::Filled { filled_qty, .. } => *filled_qty,
            OrderStatus::Cancelled { filled_qty } => *filled_qty,
            _ => Decimal::ZERO,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: OrderId,
    pub instrument_id: String,
    /// 0.6.0 新增:结构化 instrument 标识(spot / swap),与 `instrument_id` 并存。
    /// 老 snapshot / 老调用方不传时 = `None`,运行期用 `instrument_id` 字符串作为 fallback;
    /// 新调用方应 `with_instrument(...)` 显式注入,供跨 leg 风险约束 / 路由使用。
    #[serde(default)]
    pub instrument: Option<Instrument>,
    pub side: Side,
    pub order_type: OrderType,
    pub quantity: Decimal,
    pub price: Decimal,
    pub status: OrderStatus,
    pub idempotency_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub meta: HashMap<String, String>,
}

impl Order {
    pub fn new(
        instrument_id: String,
        side: Side,
        order_type: OrderType,
        quantity: Decimal,
        price: Decimal,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: OrderId::new(),
            instrument_id,
            instrument: None,
            side,
            order_type,
            quantity,
            price,
            status: OrderStatus::New,
            idempotency_key: None,
            created_at: now,
            updated_at: now,
            meta: HashMap::new(),
        }
    }

    /// 0.6.0 新增:注入结构化 instrument(spot / swap),与 `instrument_id` 同步但
    /// 保留 `instrument_id` 用于序列化 / 旧 OMS 兼容。
    pub fn with_instrument(mut self, instrument: Instrument) -> Self {
        self.instrument = Some(instrument);
        self
    }

    pub fn with_idempotency_key(mut self, key: String) -> Self {
        self.idempotency_key = Some(key);
        self
    }

    pub fn transition(&mut self, new_status: OrderStatus) -> Result<(), crate::error::OmsError> {
        // Stage B-MVP:状态机完全由 can_transition_to 定义(包括 fill event 失败的
        // reverse 回滚路径 Filled/PartiallyFilled → Acknowledged),不再做 is_terminal
        // 短路检查(原检查与 can_transition_to 重复,且会阻塞合法的反向回滚)。
        if !self.status.can_transition_to(&new_status) {
            return Err(crate::error::OmsError::InvalidTransition {
                from: format!("{:?}", self.status),
                to: format!("{:?}", new_status),
            });
        }
        self.status = new_status;
        self.updated_at = Utc::now();
        Ok(())
    }

    pub fn remaining_qty(&self) -> Decimal {
        self.quantity - self.status.filled_quantity()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub fill_id: String,
    /// 交易 symbol(如 "BTC-USDT"),由 fill event 携带
    pub symbol: String,
    /// 0.6.0 新增:结构化 instrument(spot / swap),与 `symbol` 字符串并存。
    /// 老 fill 事件无此字段时 = `None`,路由 / 风险约束用 `symbol` 字符串作 fallback。
    #[serde(default)]
    pub instrument: Option<Instrument>,
    pub price: Decimal,
    pub quantity: Decimal,
    pub fee: Decimal,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRecord {
    pub order: Order,
    pub fills: Vec<Fill>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// OMS 完整快照 — 扩展为含 portfolio 段
///
/// **向后兼容**:portfolio 字段为 `Option`,老 snapshot(无此字段)反序列化时 serde 用 `default` = None,
/// recover 路径识别 None 时创建空 portfolio(允许老 OMS 进程在升级后继续 recover)。
///
/// **0.6.0 版本号约定**:`version` 字段含义:
/// - `1` = Stage B-MVP(0.5.0 之前,无 `Order::instrument` / `Fill::instrument` 字段)
/// - `2` = 0.6.0+(`Order` / `Fill` 携带可选 `instrument: Option<Instrument>` 字段)
///
/// `recover` 路径不强制要求 version 匹配 — 老 snapshot(v1)在 0.6.0 进程里
/// 仍可反序列化(`#[serde(default)]` 让 `instrument` 字段缺省 = `None`),
/// 新 snapshot(v2)在老进程里会被 serde 拒绝(`instrument` 字段未知)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmsSnapshot {
    pub active_orders: HashMap<OrderId, Order>,
    pub order_history: Vec<OrderRecord>,
    pub version: u64,
    pub timestamp: DateTime<Utc>,
    /// Stage B-MVP 新增 — None 表示老 snapshot(回退到空 portfolio)
    #[serde(default)]
    pub portfolio: Option<PortfolioSnapshot>,
}

/// 0.6.0 新增:`OmsSnapshot::version` 当前写出版本号。
///
/// 调用 `OrderManager::snapshot()` 时,新 snapshot 的 `version` 自动设为该常量。
/// 老 snapshot(v1) 由 0.5.0 之前的 OMS 写入。
pub const OMS_SNAPSHOT_VERSION_CURRENT: u64 = 2;
/// 0.5.0 之前的 OMS 写入的 snapshot 的 `version` 起始值(1)。
pub const OMS_SNAPSHOT_VERSION_LEGACY: u64 = 1;

// Stage B-MVP 新增 — Portfolio 子结构导出
pub use crate::portfolio::{PortfolioSnapshot, Position};
