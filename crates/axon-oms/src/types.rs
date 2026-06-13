use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

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

    pub fn with_idempotency_key(mut self, key: String) -> Self {
        self.idempotency_key = Some(key);
        self
    }

    pub fn transition(&mut self, new_status: OrderStatus) -> Result<(), crate::error::OmsError> {
        if self.status.is_terminal() {
            return Err(crate::error::OmsError::AlreadyTerminal(format!(
                "{:?}",
                self.status
            )));
        }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmsSnapshot {
    pub active_orders: HashMap<OrderId, Order>,
    pub order_history: Vec<OrderRecord>,
    pub version: u64,
    pub timestamp: DateTime<Utc>,
}
