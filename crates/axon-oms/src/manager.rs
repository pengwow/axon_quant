use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use rust_decimal::Decimal;

use crate::error::OmsError;
use crate::portfolio::{Portfolio, PortfolioSnapshot};
use crate::types::{Fill, OmsSnapshot, Order, OrderId, OrderRecord, OrderStatus};

pub struct OrderManager {
    active_orders: RwLock<HashMap<OrderId, Order>>,
    order_history: RwLock<Vec<OrderRecord>>,
    idempotency_index: RwLock<HashMap<String, OrderId>>,
    snapshot_version: RwLock<u64>,
    /// Stage B-MVP 新增 — 共享 portfolio(订单层 -> 资产层的事件消费方)
    portfolio: Arc<RwLock<Portfolio>>,
}

impl OrderManager {
    pub fn new() -> Self {
        Self {
            active_orders: RwLock::new(HashMap::new()),
            order_history: RwLock::new(Vec::new()),
            idempotency_index: RwLock::new(HashMap::new()),
            snapshot_version: RwLock::new(0),
            portfolio: Arc::new(RwLock::new(Portfolio::new())),
        }
    }

    /// 初始存款(OMS 启动时调)
    pub fn deposit(&self, currency: &str, amount: Decimal) {
        self.portfolio.write().deposit(currency, amount);
    }

    /// 余额快照(供 TradingBackend::get_balance)
    pub fn snapshot_balance(&self) -> PortfolioSnapshot {
        self.portfolio.read().snapshot()
    }

    /// 持仓快照(供 TradingBackend::get_positions)
    pub fn snapshot_positions(&self) -> Vec<crate::types::Position> {
        self.portfolio.read().positions.values().cloned().collect()
    }

    pub fn submit(&self, mut order: Order) -> Result<OrderId, OmsError> {
        if let Some(ref key) = order.idempotency_key {
            let index = self.idempotency_index.read();
            if index.contains_key(key) {
                return Err(OmsError::DuplicateIdempotencyKey(key.clone()));
            }
        }

        order.status = OrderStatus::Submitted;
        let now = Utc::now();
        order.updated_at = now;
        let order_id = order.id;

        if let Some(ref key) = order.idempotency_key {
            self.idempotency_index.write().insert(key.clone(), order_id);
        }

        self.active_orders.write().insert(order_id, order.clone());
        self.order_history.write().push(OrderRecord {
            order,
            fills: Vec::with_capacity(4), // 预分配 4 个 fill 的空间
            created_at: now,
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

    /// 处理一个 fill:状态机转移 + emit fill event 给 portfolio
    ///
    /// **Lock pattern(per spec §5.3)**:
    /// 1. 取 active_orders 写锁,做状态机转移(保存 prev_status)
    /// 2. 释放 active_orders 写锁
    /// 3. 取 portfolio 写锁,apply_fill
    /// 4. portfolio 失败 → 重新取 active_orders 写锁,best-effort reverse 状态机
    /// 5. push to order_history
    ///
    /// **锁不重叠**避免死锁;**best-effort reverse**:若 order 已被并发
    /// cancel(不在 active 集合),跳过 reverse 但仍返回 Err(上游决定如何处理)。
    pub fn add_fill(&self, order_id: OrderId, fill: Fill) -> Result<(), OmsError> {
        // 1. 状态机转移
        let prev_status: OrderStatus;
        let new_status: OrderStatus = {
            let mut orders = self.active_orders.write();
            let order = orders
                .get_mut(&order_id)
                .ok_or_else(|| OmsError::OrderNotFound(order_id.to_string()))?;
            prev_status = order.status.clone();

            let new_filled = order.status.filled_quantity() + fill.quantity;
            let next = if new_filled >= order.quantity {
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
            order.transition(next.clone())?;
            next
        };
        // active_orders 锁已释放

        // 2. emit fill event 给 portfolio
        if let Err(e) = self.portfolio.write().apply_fill(&fill) {
            // 3. best-effort reverse 状态机
            let mut orders = self.active_orders.write();
            if let Some(order) = orders.get_mut(&order_id) {
                // 仅当 order.status == new_status(未被并发操作改变)时 reverse
                if order.status == new_status {
                    let _ = order.transition(prev_status);
                }
            }
            return Err(OmsError::Portfolio(e.to_string()));
        }
        // portfolio 锁已释放

        // 4. push to order_history
        {
            let mut history = self.order_history.write();
            if let Some(record) = history.iter_mut().rfind(|r| r.order.id == order_id) {
                record.order.status = new_status.clone();
                record.fills.push(fill.clone());
            }
        }
        Ok(())
    }

    /// 查看订单当前状态(测试用)
    pub fn get_order_status(&self, id: OrderId) -> Option<OrderStatus> {
        self.active_orders.read().get(&id).map(|o| o.status.clone())
    }

    pub fn snapshot(&self) -> OmsSnapshot {
        let mut version = self.snapshot_version.write();
        *version += 1;
        OmsSnapshot {
            active_orders: self.active_orders.read().clone(),
            order_history: self.order_history.read().clone(),
            version: *version,
            timestamp: Utc::now(),
            portfolio: Some(self.portfolio.read().snapshot()),
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

        // portfolio 段恢复:None = 空 portfolio(老 snapshot 兼容)
        let mut portfolio = self.portfolio.write();
        *portfolio = snapshot
            .portfolio
            .map(Portfolio::from_snapshot)
            .unwrap_or_default();
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
        // 注:Stage B-MVP 后 add_fill 不再从 active_orders 移除已 Filled 订单
        // (设计:filled 订单保留在 active 直到显式清理,状态机 Filled 即为终态信号)。
        // 故此处仅断言 add_fill 成功 + portfolio 收到事件,不再断言 active_count == 0。
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(10000));
        let order = make_order(Side::Buy, dec!(0.1), dec!(65000));
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();

        let fill = Fill {
            fill_id: "f1".into(),
            symbol: "BTC-USDT".into(),
            price: dec!(65000),
            quantity: dec!(0.1),
            fee: dec!(6.5),
            timestamp: Utc::now(),
        };
        oms.add_fill(id, fill).unwrap();
        // portfolio 收到 fill 事件(0.1 * 65000 + 6.5 = 6506.5 USDT 消耗)
        assert_eq!(oms.snapshot_balance().cash.get("USDT"), Some(&dec!(3493.5)));
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

    #[test]
    fn deposit_updates_portfolio() {
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(100000));
        let bal = oms.snapshot_balance();
        assert_eq!(bal.cash.get("USDT"), Some(&dec!(100000)));
    }

    #[test]
    fn snapshot_balance_reflects_fills() {
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(100000));
        let order = Order::new(
            "BTC-USDT".into(),
            Side::Buy,
            OrderType::Market,
            dec!(1),
            dec!(50000),
        );
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();
        oms.add_fill(
            id,
            Fill {
                fill_id: "f1".into(),
                symbol: "BTC-USDT".into(),
                price: dec!(50000),
                quantity: dec!(1),
                fee: dec!(0),
                timestamp: Utc::now(),
            },
        )
        .unwrap();
        // cash = 100000 - 50000 = 50000
        assert_eq!(oms.snapshot_balance().cash.get("USDT"), Some(&dec!(50000)));
    }

    #[test]
    fn snapshot_positions_reflects_long_position() {
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(100000));
        let order = Order::new(
            "BTC-USDT".into(),
            Side::Buy,
            OrderType::Market,
            dec!(0.5),
            dec!(50000),
        );
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();
        oms.add_fill(
            id,
            Fill {
                fill_id: "f1".into(),
                symbol: "BTC-USDT".into(),
                price: dec!(50000),
                quantity: dec!(0.5),
                fee: dec!(0),
                timestamp: Utc::now(),
            },
        )
        .unwrap();

        let positions = oms.snapshot_positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, "BTC-USDT");
        assert_eq!(positions[0].quantity, dec!(0.5));
        assert_eq!(positions[0].avg_price, dec!(50000));
    }

    #[test]
    fn add_fill_propagates_portfolio_error() {
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(100)); // 只存 100 USDT
        let order = Order::new(
            "BTC-USDT".into(),
            Side::Buy,
            OrderType::Market,
            dec!(1),
            dec!(50000),
        );
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();
        let err = oms
            .add_fill(
                id,
                Fill {
                    fill_id: "f1".into(),
                    symbol: "BTC-USDT".into(),
                    price: dec!(50000),
                    quantity: dec!(1),
                    fee: dec!(0),
                    timestamp: Utc::now(),
                },
            )
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "portfolio error: insufficient cash: need 50000 USDT, have 100"
        );
    }

    #[test]
    fn add_fill_failed_portfolio_reverses_state_machine() {
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(100));
        let order = Order::new(
            "BTC-USDT".into(),
            Side::Buy,
            OrderType::Market,
            dec!(1),
            dec!(50000),
        );
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();

        // 触发 portfolio 失败(现金不足)
        let _ = oms.add_fill(
            id,
            Fill {
                fill_id: "f1".into(),
                symbol: "BTC-USDT".into(),
                price: dec!(50000),
                quantity: dec!(1),
                fee: dec!(0),
                timestamp: Utc::now(),
            },
        );

        // 状态机应被 reverse 回 Acknowledged(partially filled 状态被撤销)
        // 验证:order 仍在 active 集合(未被 reverse 推到 history)
        let status = oms.get_order_status(id).expect("order still active");
        assert_eq!(status, OrderStatus::Acknowledged);
    }

    #[test]
    fn cancel_does_not_change_portfolio() {
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(100000));
        let order = Order::new(
            "BTC-USDT".into(),
            Side::Buy,
            OrderType::Limit,
            dec!(0.1),
            dec!(50000),
        );
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();
        // 仅比较 cash + positions(as_of 是 Utc::now,不可比)
        let cash_before = oms.snapshot_balance().cash;
        oms.cancel(id).unwrap();
        assert_eq!(oms.snapshot_balance().cash, cash_before);
        assert!(oms.snapshot_positions().is_empty());
    }

    #[test]
    fn reject_does_not_change_portfolio() {
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(100000));
        let order = Order::new(
            "BTC-USDT".into(),
            Side::Buy,
            OrderType::Limit,
            dec!(0.1),
            dec!(50000),
        );
        let id = oms.submit(order).unwrap();
        let cash_before = oms.snapshot_balance().cash;
        oms.update_status(
            id,
            OrderStatus::Rejected {
                reason: "test".into(),
            },
        )
        .unwrap();
        assert_eq!(oms.snapshot_balance().cash, cash_before);
        assert!(oms.snapshot_positions().is_empty());
    }

    #[test]
    fn concurrent_add_fills_dont_lose_updates() {
        use std::sync::Arc;
        use std::thread;

        let oms = Arc::new(OrderManager::new());
        oms.deposit("USDT", dec!(10_000_000));

        // 10 个并发 fill,每个 fill 价格不同,sum 应等于总 cash 变化
        let mut handles = vec![];
        for i in 0..10 {
            let oms = oms.clone();
            handles.push(thread::spawn(move || {
                let order = Order::new(
                    "BTC-USDT".into(),
                    Side::Buy,
                    OrderType::Market,
                    dec!(1),
                    Decimal::from(50000 + i),
                );
                let id = oms.submit(order).unwrap();
                oms.update_status(id, OrderStatus::Acknowledged).unwrap();
                oms.add_fill(
                    id,
                    Fill {
                        fill_id: format!("f{}", i),
                        symbol: "BTC-USDT".into(),
                        price: Decimal::from(50000 + i),
                        quantity: dec!(1),
                        fee: dec!(0),
                        timestamp: Utc::now(),
                    },
                )
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // 验证:cash = 10_000_000 - sum(50000..50009)= 10_000_000 - 500045 = 9_499_955
        let expected_cash = dec!(10_000_000)
            - Decimal::from(
                50000 + 50001 + 50002 + 50003 + 50004 + 50005 + 50006 + 50007 + 50008 + 50009,
            );
        assert_eq!(
            oms.snapshot_balance().cash.get("USDT"),
            Some(&expected_cash)
        );

        // 验证:positions 应有 1 个 symbol,quantity = 10(10 个 buy)
        let positions = oms.snapshot_positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].quantity, dec!(10));
    }
}
