use std::collections::HashMap;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use crate::error::ExchangeError;
use crate::types::{Order, OrderId, OrderStatus};

#[derive(Debug, Clone)]
pub struct TrackedOrder {
    pub client_order_id: OrderId,
    pub exchange_order_id: Option<String>,
    pub order: Order,
    pub status: OrderStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct OrderRecord {
    pub order: Order,
    pub final_status: OrderStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

pub struct OrderLifecycleManager {
    active_orders: RwLock<HashMap<OrderId, TrackedOrder>>,
    order_history: RwLock<Vec<OrderRecord>>,
}

impl OrderLifecycleManager {
    pub fn new() -> Self {
        Self {
            active_orders: RwLock::new(HashMap::new()),
            order_history: RwLock::new(Vec::new()),
        }
    }

    pub fn register_order(&self, order: Order) -> OrderId {
        let client_id = order.client_order_id;
        let tracked = TrackedOrder {
            client_order_id: client_id,
            exchange_order_id: None,
            order,
            status: OrderStatus::Pending,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        self.active_orders.write().insert(client_id, tracked);
        client_id
    }

    pub fn update_status(
        &self,
        order_id: OrderId,
        new_status: OrderStatus,
    ) -> Result<(), ExchangeError> {
        let mut orders = self.active_orders.write();
        let tracked = orders
            .get_mut(&order_id)
            .ok_or_else(|| ExchangeError::OrderNotFound(order_id.to_string()))?;

        tracked.status = new_status.clone();
        tracked.updated_at = Utc::now();

        if new_status.is_terminal() {
            let record = OrderRecord {
                order: tracked.order.clone(),
                final_status: new_status,
                created_at: tracked.created_at,
                completed_at: Some(Utc::now()),
            };
            orders.remove(&order_id);
            self.order_history.write().push(record);
        }

        Ok(())
    }

    pub fn active_count(&self) -> usize {
        self.active_orders.read().len()
    }

    pub fn history_count(&self) -> usize {
        self.order_history.read().len()
    }
}

impl Default for OrderLifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ExchangeId, Side, Symbol, TimeInForce};
    use rust_decimal::Decimal;

    fn make_order() -> Order {
        Order {
            client_order_id: OrderId::new(),
            symbol: Symbol::new("BTCUSDT"),
            side: Side::Buy,
            order_type: crate::types::OrderType::Limit,
            price: Some(Decimal::from(50000)),
            quantity: Decimal::new(1, 3),
            time_in_force: TimeInForce::Gtc,
            exchange: ExchangeId::Binance,
            meta: HashMap::new(),
        }
    }

    #[test]
    fn test_register_order() {
        let manager = OrderLifecycleManager::new();
        let order = make_order();
        let _id = order.client_order_id;
        manager.register_order(order);
        assert_eq!(manager.active_count(), 1);
    }

    #[test]
    fn test_update_to_terminal() {
        let manager = OrderLifecycleManager::new();
        let order = make_order();
        let id = order.client_order_id;
        manager.register_order(order);
        manager
            .update_status(
                id,
                OrderStatus::Filled {
                    filled_qty: Decimal::new(1, 3),
                    avg_price: Decimal::from(50000),
                },
            )
            .unwrap();
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.history_count(), 1);
    }
}
