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
    /// Stage B-MVP 新增 — 共享 portfolio(订单层 -> 资产层的事件消费方)
    portfolio: Arc<RwLock<Portfolio>>,
}

impl OrderManager {
    pub fn new() -> Self {
        Self {
            active_orders: RwLock::new(HashMap::new()),
            order_history: RwLock::new(Vec::new()),
            idempotency_index: RwLock::new(HashMap::new()),
            portfolio: Arc::new(RwLock::new(Portfolio::new())),
        }
    }

    /// 初始存款(OMS 启动时调)
    pub fn deposit(&self, currency: &str, amount: Decimal) {
        self.portfolio.write().deposit(currency, amount);
    }

    /// 切换账务模式(`Cash` 现货 / `Wallet` 合约钱包)
    ///
    /// 只影响后续 `add_fill` 的现金流口径;合约账户必须用 `Wallet`,
    /// 否则现金会按全额名义价值被扣减,与交易所钱包余额对不上。
    pub fn set_accounting_mode(&self, mode: crate::portfolio::AccountingMode) {
        self.portfolio.write().set_accounting_mode(mode);
    }

    /// 注入初始持仓（adopt 接管）：合并覆盖同 symbol，不触碰 cash
    ///
    /// 供 live 启动把交易所残留持仓抄进影子账本；symbol 用 `BASE/QUOTE` 规范键，
    /// quantity 带符号（正 = 多，负 = 空）。cash 由 `deposit` 单独钉住，这里绝不改它。
    pub fn seed_initial_positions(&self, positions: Vec<crate::types::Position>) {
        let mut portfolio = self.portfolio.write();
        for position in positions {
            portfolio
                .positions
                .insert(position.symbol.clone(), position);
        }
    }

    /// 取出现金(出金),余额不足时返回错误
    pub fn withdraw(
        &self,
        currency: &str,
        amount: Decimal,
    ) -> Result<(), crate::portfolio::PortfolioError> {
        self.portfolio.write().withdraw(currency, amount)
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
        let (new_status, fill) = {
            let mut orders = self.active_orders.write();
            let order = orders
                .get_mut(&order_id)
                .ok_or_else(|| OmsError::OrderNotFound(order_id.to_string()))?;
            prev_status = order.status.clone();

            // 超量回报钳制：fill 数量最多补齐剩余量，防止异常回报把 filled_qty
            // 推过 order.quantity（现金 / 仓位会被多记）。被钳掉的超量部分不是
            // 丢弃，而是留给上层对账（reconcile 的成交/持仓差集）去暴露。钳制
            // 后的 fill 贯穿后续记账，保证状态机、portfolio、history 三处一致。
            // 已满订单（remaining == 0）再来的回报状态机会拒（Filled 为终态）。
            //
            // 符号契约：全仓 Fill.quantity 带符号（买正卖负，portfolio 记账依赖
            // 方向），而 order.quantity / filled_qty 均为绝对值 —— 状态机数学
            // 只取绝对数量，钳制时保留方向符号（卖单超量钳到 -remaining）。
            let remaining = order.quantity - order.status.filled_quantity();
            let mut fill = fill;
            let fill_qty_abs = fill.quantity.abs();
            if fill_qty_abs > remaining {
                fill.quantity = if fill.quantity.is_sign_negative() {
                    -remaining
                } else {
                    remaining
                };
            }

            let new_filled = order.status.filled_quantity() + fill.quantity.abs();
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
            (next, fill)
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
        // 0.6.0 改:`version` 字段不再自增,直接写 `OMS_SNAPSHOT_VERSION_CURRENT`(= 2),
        // 老 OMS 写出的 version=1 的 snapshot 仍能 recover(字段含义 = 序列化 schema 版本)。
        OmsSnapshot {
            active_orders: self.active_orders.read().clone(),
            order_history: self.order_history.read().clone(),
            version: crate::types::OMS_SNAPSHOT_VERSION_CURRENT,
            timestamp: Utc::now(),
            portfolio: Some(self.portfolio.read().snapshot()),
        }
    }

    /// 从快照恢复订单簿与 portfolio
    ///
    /// 快照不携带账务模式,恢复后 `Portfolio` 回到默认 `Cash`;合约调用方需要在
    /// `recover` 之后重新调用 [`Self::set_accounting_mode`]。
    pub fn recover(&self, snapshot: OmsSnapshot) -> Result<(), OmsError> {
        *self.active_orders.write() = snapshot.active_orders;
        *self.order_history.write() = snapshot.order_history;
        // 0.6.0 改:`version` 不再回写到内部计数器(recover 不再维护自增 version)。
        // 这里仅做参数兼容,真正语义见 `OMS_SNAPSHOT_VERSION_CURRENT` 注释。
        let _ = snapshot.version;

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
    use crate::types::{OrderType, Position, Side};
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
            instrument: None,
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
    fn test_add_fill_clamps_overfill() {
        // 超量回报钳制：fill 0.12 > 剩余 0.1 → 按 0.1 记账，filled_qty 不越过订单数量
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(10000));
        let order = make_order(Side::Buy, dec!(0.1), dec!(65000));
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();

        let fill = Fill {
            fill_id: "f-over".into(),
            symbol: "BTC-USDT".into(),
            instrument: None,
            price: dec!(65000),
            quantity: dec!(0.12),
            fee: dec!(7.8),
            timestamp: Utc::now(),
        };
        oms.add_fill(id, fill).unwrap();

        match oms.get_order_status(id) {
            Some(OrderStatus::Filled { filled_qty, .. }) => {
                assert_eq!(filled_qty, dec!(0.1), "超量部分被钳制到剩余量");
            }
            other => panic!("应转为 Filled，实际 {other:?}"),
        }
        // portfolio 只收到钳制后的记账：0.1 * 65000 + 7.8 = 6507.8
        assert_eq!(oms.snapshot_balance().cash.get("USDT"), Some(&dec!(3492.2)));
    }

    #[test]
    fn test_fill_sell_order_fully() {
        // 回归：全仓 Fill.quantity 带符号（卖单为负），状态机必须按绝对值累计。
        // 曾因直接比较带符号值导致卖单全额成交（-0.1 >= 0.1 为假）永远卡
        // PartiallyFilled。
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(0.001));
        let order = make_order(Side::Sell, dec!(0.1), dec!(65000));
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();

        let fill = Fill {
            fill_id: "f-sell".into(),
            symbol: "BTC-USDT".into(),
            instrument: None,
            price: dec!(65000),
            quantity: dec!(-0.1),
            fee: dec!(6.5),
            timestamp: Utc::now(),
        };
        oms.add_fill(id, fill).unwrap();

        match oms.get_order_status(id) {
            Some(OrderStatus::Filled { filled_qty, .. }) => {
                assert_eq!(filled_qty, dec!(0.1), "卖单全额成交，filled_qty 为绝对值");
            }
            other => panic!("卖单全额成交应转为 Filled，实际 {other:?}"),
        }
    }

    #[test]
    fn test_add_fill_clamps_sell_overfill() {
        // 回归：卖单超量回报（-0.12 > 剩余 0.1 的绝对量）也必须被钳制，
        // 且钳制保留符号（-0.1 而非 +0.1，否则 portfolio 记账方向翻转）。
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(0.001));
        let order = make_order(Side::Sell, dec!(0.1), dec!(65000));
        let id = oms.submit(order).unwrap();
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();

        let fill = Fill {
            fill_id: "f-sell-over".into(),
            symbol: "BTC-USDT".into(),
            instrument: None,
            price: dec!(65000),
            quantity: dec!(-0.12),
            fee: dec!(7.8),
            timestamp: Utc::now(),
        };
        oms.add_fill(id, fill).unwrap();

        match oms.get_order_status(id) {
            Some(OrderStatus::Filled { filled_qty, .. }) => {
                assert_eq!(filled_qty, dec!(0.1), "卖单超量部分被钳到剩余量的绝对值");
            }
            other => panic!("应转为 Filled，实际 {other:?}"),
        }
        // 卖出 0.1 BTC 收款 6500，手续费 7.8，加初始 0.001 → USDT 余额 6492.201（方向未被翻转）
        assert_eq!(
            oms.snapshot_balance().cash.get("USDT"),
            Some(&dec!(6492.201))
        );
        // portfolio 是持仓模型：卖单开空仓 -0.1，不动现金里的 BTC
        let positions = oms.snapshot_positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, "BTC-USDT");
        assert_eq!(positions[0].quantity, dec!(-0.1));
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
        // 0.6.0 改:version 不再自增,固定为 `OMS_SNAPSHOT_VERSION_CURRENT`(= 2)
        assert_eq!(snapshot.version, crate::types::OMS_SNAPSHOT_VERSION_CURRENT);
        assert_eq!(snapshot.version, 2);

        let oms2 = OrderManager::new();
        oms2.recover(snapshot).unwrap();
        assert_eq!(oms2.active_count(), 1);
    }

    /// 0.6.0 新增:`Order::with_instrument` 注入结构化 instrument 后,
    /// snapshot / recover 路径保留该字段(往返无损)。
    #[test]
    fn test_order_with_instrument_roundtrip() {
        use axon_core::types::{SpotInstrument, Symbol};
        let oms = OrderManager::new();
        let btc = axon_core::Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        });
        let order = make_order(Side::Buy, dec!(0.1), dec!(65000)).with_instrument(btc.clone());
        oms.submit(order).unwrap();

        let snap = oms.snapshot();
        let recovered: Vec<_> = snap
            .active_orders
            .values()
            .filter_map(|o| o.instrument.clone())
            .collect();
        assert_eq!(recovered, vec![btc]);
    }

    /// 0.6.0 新增:无 `instrument` 字段的 Order snapshot 仍能 recover
    /// (`#[serde(default)]` 让字段缺省 = `None`)。
    #[test]
    fn test_legacy_snapshot_without_instrument_recovers() {
        use crate::types::OMS_SNAPSHOT_VERSION_LEGACY;
        let oms = OrderManager::new();
        oms.submit(make_order(Side::Buy, dec!(0.1), dec!(65000)))
            .unwrap();

        let mut snap = oms.snapshot();
        // 模拟 0.5.0 之前写出的老 snapshot:version=1,所有 Order.instrument = None
        snap.version = OMS_SNAPSHOT_VERSION_LEGACY;
        for o in snap.active_orders.values_mut() {
            o.instrument = None;
        }

        let oms2 = OrderManager::new();
        oms2.recover(snap).unwrap();
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
                instrument: None,
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
                instrument: None,
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
                    instrument: None,
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
                instrument: None,
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
                        instrument: None,
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

    /// seed_initial_positions：注入持仓只动 positions，不碰 cash
    #[test]
    fn seed_initial_positions_inserts_positions() {
        let oms = OrderManager::new();
        oms.deposit("USDT", dec!(10_000));
        oms.seed_initial_positions(vec![Position {
            symbol: "BTC/USDT".into(),
            quantity: dec!(-0.5),
            avg_price: dec!(100.25),
            realized_pnl: Decimal::ZERO,
            updated_at: Utc::now(),
        }]);

        let positions = oms.snapshot_positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, "BTC/USDT");
        assert_eq!(positions[0].quantity, dec!(-0.5));
        assert_eq!(positions[0].avg_price, dec!(100.25));
        // cash 不受注入影响（基线由 deposit 单独钉住）
        assert_eq!(oms.snapshot_balance().cash.get("USDT"), Some(&dec!(10_000)));
    }

    /// 同 symbol 重复注入按覆盖合并（接管/重放幂等，不叠加）
    #[test]
    fn seed_initial_positions_overwrites_same_symbol() {
        let oms = OrderManager::new();
        let make = |quantity: Decimal| Position {
            symbol: "BTC/USDT".into(),
            quantity,
            avg_price: dec!(100),
            realized_pnl: Decimal::ZERO,
            updated_at: Utc::now(),
        };
        oms.seed_initial_positions(vec![make(dec!(1))]);
        oms.seed_initial_positions(vec![make(dec!(2))]);

        let positions = oms.snapshot_positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].quantity, dec!(2));
    }
}
