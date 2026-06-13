use std::collections::HashMap;

use chrono::Utc;
use parking_lot::RwLock;

use crate::error::OmsError;
use crate::types::{Fill, OmsSnapshot, Order, OrderId, OrderRecord, OrderStatus};

pub struct OrderManager {
    active_orders: RwLock<HashMap<OrderId, Order>>,
    order_history: RwLock<Vec<OrderRecord>>,
    idempotency_index: RwLock<HashMap<String, OrderId>>,
    snapshot_version: RwLock<u64>,
}

impl OrderManager {
    pub fn new() -> Self {
        Self {
            active_orders: RwLock::new(HashMap::new()),
            order_history: RwLock::new(Vec::new()),
            idempotency_index: RwLock::new(HashMap::new()),
            snapshot_version: RwLock::new(0),
        }
    }

    pub fn submit(&self, mut order: Order) -> Result<OrderId, OmsError> {
        if let Some(ref key) = order.idempotency_key {
            let index = self.idempotency_index.read();
            if index.contains_key(key) {
                return Err(OmsError::DuplicateIdempotencyKey(key.clone()));
            }
        }

        order.status = OrderStatus::Submitted;
        order.updated_at = Utc::now();
        let order_id = order.id;

        if let Some(ref key) = order.idempotency_key {
            self.idempotency_index.write().insert(key.clone(), order_id);
        }

        self.active_orders.write().insert(order_id, order.clone());
        self.order_history.write().push(OrderRecord {
            order,
            fills: Vec::new(),
            created_at: Utc::now(),
            completed_at: None,
        });

        Ok(order_id)
    }

    pub fn cancel(&self, order_id: OrderId) -> Result<(), OmsError> {
        let mut orders = self.active_orders.write();
        let order = orders
            .get_mut(&order_id)
            .ok_or_else(|| OmsError::OrderNotFound(order_id.to_string()))?;

        let filled_qty = order.status.filled_quantity();
        order.transition(OrderStatus::Cancelled { filled_qty })?;

        let order = orders.remove(&order_id).unwrap();
        if let Some(record) = self
            .order_history
            .write()
            .iter_mut()
            .rfind(|r| r.order.id == order_id)
        {
            record.order = order;
            record.completed_at = Some(Utc::now());
        }

        Ok(())
    }

    pub fn update_status(
        &self,
        order_id: OrderId,
        new_status: OrderStatus,
    ) -> Result<(), OmsError> {
        let mut orders = self.active_orders.write();
        let order = orders
            .get_mut(&order_id)
            .ok_or_else(|| OmsError::OrderNotFound(order_id.to_string()))?;

        order.transition(new_status.clone())?;

        if new_status.is_terminal() {
            let order = orders.remove(&order_id).unwrap();
            if let Some(record) = self
                .order_history
                .write()
                .iter_mut()
                .rfind(|r| r.order.id == order_id)
            {
                record.order = order;
                record.completed_at = Some(Utc::now());
            }
        }

        Ok(())
    }

    pub fn add_fill(&self, order_id: OrderId, fill: Fill) -> Result<(), OmsError> {
        let mut orders = self.active_orders.write();
        let order = orders
            .get_mut(&order_id)
            .ok_or_else(|| OmsError::OrderNotFound(order_id.to_string()))?;

        let new_filled = order.status.filled_quantity() + fill.quantity;
        let new_status = if new_filled >= order.quantity {
            OrderStatus::Filled {
                filled_qty: new_filled,
                avg_price: fill.price,
            }
        } else {
            OrderStatus::PartiallyFilled {
                filled_qty: new_filled,
                avg_price: fill.price,
            }
        };

        order.transition(new_status.clone())?;

        if new_status.is_terminal() {
            let order = orders.remove(&order_id).unwrap();
            if let Some(record) = self
                .order_history
                .write()
                .iter_mut()
                .rfind(|r| r.order.id == order_id)
            {
                record.order = order;
                record.fills.push(fill);
            }
        } else {
            if let Some(record) = self
                .order_history
                .write()
                .iter_mut()
                .rfind(|r| r.order.id == order_id)
            {
                record.order = order.clone();
                record.fills.push(fill);
            }
        }

        Ok(())
    }

    pub fn snapshot(&self) -> OmsSnapshot {
        let mut version = self.snapshot_version.write();
        *version += 1;
        OmsSnapshot {
            active_orders: self.active_orders.read().clone(),
            order_history: self.order_history.read().clone(),
            version: *version,
            timestamp: Utc::now(),
        }
    }

    pub fn recover(&self, snapshot: OmsSnapshot) -> Result<(), OmsError> {
        *self.active_orders.write() = snapshot.active_orders;
        *self.order_history.write() = snapshot.order_history;
        *self.snapshot_version.write() = snapshot.version;

        self.idempotency_index.write().clear();
        let orders = self.active_orders.read();
        for (id, order) in orders.iter() {
            if let Some(ref key) = order.idempotency_key {
                self.idempotency_index.write().insert(key.clone(), *id);
            }
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

impl Default for OrderManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OrderType, Side};
    use pretty_assertions::assert_eq;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    fn make_order(side: Side, qty: Decimal, price: Decimal) -> Order {
        Order::new("BTC-USDT".into(), side, OrderType::Limit, qty, price)
    }

    #[test]
    fn test_submit_order() {
        let oms = OrderManager::new();
        let order = make_order(Side::Buy, dec!(0.1), dec!(65000));
        let _id = oms.submit(order).unwrap();
        assert_eq!(oms.active_count(), 1);
    }

    #[test]
    fn test_cancel_order() {
        let oms = OrderManager::new();
        let order = make_order(Side::Buy, dec!(0.1), dec!(65000));
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();
        oms.cancel(id).unwrap();
        assert_eq!(oms.active_count(), 0);
        assert_eq!(oms.history_count(), 1);
    }

    #[test]
    fn test_fill_order() {
        let oms = OrderManager::new();
        let order = make_order(Side::Buy, dec!(0.1), dec!(65000));
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();

        let fill = Fill {
            fill_id: "f1".into(),
            price: dec!(65000),
            quantity: dec!(0.1),
            fee: dec!(6.5),
            timestamp: Utc::now(),
        };
        oms.add_fill(id, fill).unwrap();
        assert_eq!(oms.active_count(), 0);
    }

    #[test]
    fn test_idempotency() {
        let oms = OrderManager::new();
        let order =
            make_order(Side::Buy, dec!(0.1), dec!(65000)).with_idempotency_key("key1".into());
        oms.submit(order).unwrap();

        let order2 =
            make_order(Side::Buy, dec!(0.1), dec!(65000)).with_idempotency_key("key1".into());
        assert!(matches!(
            oms.submit(order2),
            Err(OmsError::DuplicateIdempotencyKey(_))
        ));
    }

    #[test]
    fn test_invalid_transition() {
        let oms = OrderManager::new();
        let order = make_order(Side::Buy, dec!(0.1), dec!(65000));
        let id = oms.submit(order).unwrap();

        assert!(matches!(
            oms.update_status(
                id,
                OrderStatus::Filled {
                    filled_qty: dec!(0.1),
                    avg_price: dec!(65000)
                }
            ),
            Err(OmsError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn test_snapshot_and_recover() {
        let oms = OrderManager::new();
        let order = make_order(Side::Buy, dec!(0.1), dec!(65000));
        oms.submit(order).unwrap();

        let snapshot = oms.snapshot();
        assert_eq!(snapshot.version, 1);

        let oms2 = OrderManager::new();
        oms2.recover(snapshot).unwrap();
        assert_eq!(oms2.active_count(), 1);
    }

    #[test]
    fn test_concurrent_submit() {
        use std::sync::Arc;

        let oms = Arc::new(OrderManager::new());
        let mut handles = vec![];

        for i in 0..10 {
            let oms = oms.clone();
            handles.push(std::thread::spawn(move || {
                let price = Decimal::from(65000 + i);
                let order = Order::new(
                    "BTC-USDT".into(),
                    Side::Buy,
                    OrderType::Limit,
                    dec!(0.1),
                    price,
                );
                oms.submit(order).unwrap()
            }));
        }

        let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(ids.len(), 10);
        assert_eq!(oms.active_count(), 10);
    }
}
