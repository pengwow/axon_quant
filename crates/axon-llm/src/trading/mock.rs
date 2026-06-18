//! Mock 后端:用于单测 / demo,与真实交易所完全隔离
//!
//! 使用方按需在自己 crate 实现 `TradingBackend` 适配真实交易所 / OMS / 回测引擎。

use std::collections::HashSet;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::trading::backend::{TradingBackend, TradingError};
use crate::trading::types::{
    BalanceSnapshot, CurrencyBalance, OrderAck, OrderKind, OrderSide, OrderStatus, PlaceOrderArgs,
    PositionSnapshot,
};

/// 测试用错误注入器
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FailureInjector {
    /// place_order 时返回的错误(优先于正常路径)
    pub place_order_error: Option<String>,
    /// get_balance 时返回的错误
    pub get_balance_error: Option<String>,
    /// get_positions 时返回的错误
    pub get_positions_error: Option<String>,
}

impl FailureInjector {
    /// 构造空注入器
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入 place_order 错误
    pub fn with_place_order_error(mut self, msg: impl Into<String>) -> Self {
        self.place_order_error = Some(msg.into());
        self
    }
}

/// Mock 后端:内存中维护 orders / balance / positions
pub struct MockTradingBackend {
    /// 历史已下订单(含未成交)
    pub orders: StdMutex<Vec<OrderAck>>,
    /// 当前余额
    pub balance: StdMutex<BalanceSnapshot>,
    /// 当前持仓
    pub positions: StdMutex<Vec<PositionSnapshot>>,
    /// 下一个订单 ID 自增器
    next_id: StdMutex<u64>,
    /// 错误注入器(测试 / 演示)
    pub failure_injector: StdMutex<FailureInjector>,
    /// 已取消订单 ID 集合(Stage E 新增)
    pub cancelled_ids: StdMutex<HashSet<String>>,
    /// 已改单订单 ID 集合(Stage E 新增)
    pub replaced_ids: StdMutex<HashSet<String>>,
    /// 累计撤单次数(Stage E 新增,公开 Mutex 便于测试断言)
    pub cancel_count: StdMutex<u32>,
}

impl Default for MockTradingBackend {
    fn default() -> Self {
        Self {
            orders: StdMutex::new(Vec::new()),
            balance: StdMutex::new(BalanceSnapshot {
                currencies: vec![
                    CurrencyBalance {
                        currency: "USDT".into(),
                        free: 10_000.0,
                        locked: 0.0,
                    },
                    CurrencyBalance {
                        currency: "BTC".into(),
                        free: 0.1,
                        locked: 0.0,
                    },
                ],
                as_of_ms: 1_700_000_000_000,
            }),
            positions: StdMutex::new(vec![PositionSnapshot {
                symbol: "BTC-USDT".into(),
                quantity: 0.1,
                entry_price: 50_000.0,
                unrealized_pnl: 100.0,
                as_of_ms: 1_700_000_000_000,
            }]),
            next_id: StdMutex::new(0),
            failure_injector: StdMutex::new(FailureInjector::default()),
            cancelled_ids: StdMutex::new(HashSet::new()),
            replaced_ids: StdMutex::new(HashSet::new()),
            cancel_count: StdMutex::new(0),
        }
    }
}

impl MockTradingBackend {
    /// 默认 mock 后端(10000 USDT + 0.1 BTC 持仓)
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前已下订单数(单测断言用)
    pub fn order_count(&self) -> usize {
        self.orders.lock().expect("poisoned").len()
    }

    /// 拆分 "BASE-QUOTE" 形式的交易对,失败回退 ("<symbol>", "USDT")
    fn split_symbol(symbol: &str) -> (&str, &str) {
        symbol.split_once('-').unwrap_or((symbol, "USDT"))
    }

    /// 成交流水:按方向调整对应币种余额与持仓
    ///
    /// 价格缺省时使用 50_000.0(与默认持仓 entry_price 对齐,保证后续查询稳定)。
    fn apply_fill(&self, symbol: &str, side: OrderSide, quantity: f64, price: Option<f64>) {
        let (base, quote) = Self::split_symbol(symbol);
        let price = price.unwrap_or(50_000.0);
        let notional = quantity * price;
        let sign = if matches!(side, OrderSide::Buy) {
            1.0
        } else {
            -1.0
        };

        // 调整余额:买入用 quote 付,收 base;卖出反之
        self.adjust_currency(quote, -sign * notional);
        self.adjust_currency(base, sign * quantity);

        // 调整持仓:买入加多,卖出减多(不区分多空,统一按净持仓)
        self.adjust_position(symbol, sign * quantity, price);
    }

    /// 调整指定币种 free 余额,若不存在则 push 新条目
    fn adjust_currency(&self, currency: &str, delta: f64) {
        let mut b = self.balance.lock().expect("poisoned");
        if let Some(c) = b.currencies.iter_mut().find(|c| c.currency == currency) {
            c.free += delta;
        } else {
            b.currencies.push(CurrencyBalance {
                currency: currency.to_string(),
                free: delta,
                locked: 0.0,
            });
        }
    }

    /// 调整指定 symbol 的净持仓:delta > 0 加仓,< 0 减仓
    fn adjust_position(&self, symbol: &str, delta: f64, price: f64) {
        let mut p = self.positions.lock().expect("poisoned");
        if let Some(pos) = p.iter_mut().find(|pos| pos.symbol == symbol) {
            // 计算新均价:简单按加权平均
            let new_qty = pos.quantity + delta;
            if new_qty.abs() < f64::EPSILON {
                // 平仓
                p.retain(|pos| pos.symbol != symbol);
                return;
            }
            if pos.quantity > 0.0 && delta > 0.0 {
                // 加仓:更新 entry_price 为加权均价
                pos.entry_price = (pos.entry_price * pos.quantity + price * delta) / new_qty;
            }
            pos.quantity = new_qty;
        } else if delta.abs() > f64::EPSILON {
            // 开新仓
            p.push(PositionSnapshot {
                symbol: symbol.to_string(),
                quantity: delta,
                entry_price: price,
                unrealized_pnl: 0.0,
                as_of_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            });
        }
    }
}

#[async_trait]
impl TradingBackend for MockTradingBackend {
    fn name(&self) -> &str {
        "mock"
    }
    async fn place_order(&self, req: &PlaceOrderArgs) -> Result<OrderAck, TradingError> {
        if let Some(e) = self
            .failure_injector
            .lock()
            .expect("poisoned")
            .place_order_error
            .clone()
        {
            return Err(TradingError::Backend(e));
        }
        let id = {
            let mut g = self.next_id.lock().expect("poisoned");
            *g += 1;
            *g
        };
        let ack = OrderAck {
            order_id: format!("MOCK-{}", id),
            symbol: req.symbol.clone(),
            side: req.side,
            quantity: req.quantity,
            status: OrderStatus("Filled".into()),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            confirm_token: None,
        };
        self.orders.lock().expect("poisoned").push(ack.clone());
        // 成交后更新余额与持仓(否则多次下单后组合状态与实际不符)
        self.apply_fill(&req.symbol, req.side, req.quantity, req.price);
        Ok(ack)
    }

    async fn get_balance(&self) -> Result<BalanceSnapshot, TradingError> {
        if let Some(e) = self
            .failure_injector
            .lock()
            .expect("poisoned")
            .get_balance_error
            .clone()
        {
            return Err(TradingError::Backend(e));
        }
        Ok(self.balance.lock().expect("poisoned").clone())
    }

    async fn get_positions(&self) -> Result<Vec<PositionSnapshot>, TradingError> {
        if let Some(e) = self
            .failure_injector
            .lock()
            .expect("poisoned")
            .get_positions_error
            .clone()
        {
            return Err(TradingError::Backend(e));
        }
        Ok(self.positions.lock().expect("poisoned").clone())
    }

    async fn cancel_order(&self, order_id: &str) -> Result<OrderAck, TradingError> {
        // 1. 检查 order_id 是否存在
        let mut orders = self.orders.lock().expect("poisoned");
        let order = orders
            .iter_mut()
            .find(|o| o.order_id == order_id)
            .ok_or_else(|| TradingError::Backend(format!("order {} not found", order_id)))?;
        // 2. 检查是否已取消
        if self
            .cancelled_ids
            .lock()
            .expect("poisoned")
            .contains(order_id)
        {
            return Err(TradingError::Backend(format!(
                "order {} already cancelled",
                order_id
            )));
        }
        // 3. 修改状态
        order.status = OrderStatus("Cancelled".into());
        order.timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let ack = order.clone();
        // 4. 加入取消集合
        drop(orders);
        self.cancelled_ids
            .lock()
            .expect("poisoned")
            .insert(order_id.to_string());
        *self.cancel_count.lock().expect("poisoned") += 1;
        Ok(ack)
    }

    async fn replace_order(
        &self,
        order_id: &str,
        new_req: &PlaceOrderArgs,
    ) -> Result<OrderAck, TradingError> {
        // 1. 查找订单
        let mut orders = self.orders.lock().expect("poisoned");
        let order = orders
            .iter_mut()
            .find(|o| o.order_id == order_id)
            .ok_or_else(|| TradingError::Backend(format!("order {} not found", order_id)))?;
        // 2. symbol / side 必须匹配(防 LLM 误传)
        if order.symbol != new_req.symbol {
            return Err(TradingError::Backend(format!(
                "replace symbol mismatch: expected {}, got {}",
                order.symbol, new_req.symbol
            )));
        }
        if order.side != new_req.side {
            return Err(TradingError::Backend(format!(
                "replace side mismatch: expected {:?}, got {:?}",
                order.side, new_req.side
            )));
        }
        // 3. 更新可改字段
        order.quantity = new_req.quantity;
        order.status = OrderStatus("Replaced".into());
        order.timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let ack = order.clone();
        drop(orders);
        // 4. 加入改单集合
        self.replaced_ids
            .lock()
            .expect("poisoned")
            .insert(order_id.to_string());
        Ok(ack)
    }
}

// 避免未使用导入告警
#[allow(dead_code)]
const _SIDES: (OrderSide, OrderKind) = (OrderSide::Buy, OrderKind::Limit);

// ── 测试 ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::types::TimeInForce;

    fn mk_args() -> PlaceOrderArgs {
        PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.05,
            order_type: OrderKind::Limit,
            price: Some(50_000.0),
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        }
    }

    /// 辅助构造器:替换 failure_injector
    fn with_failure_injector(m: MockTradingBackend, fi: FailureInjector) -> MockTradingBackend {
        *m.failure_injector.lock().expect("poisoned") = fi;
        m
    }

    /// Stage H:Mock 后端 name() 标签
    #[tokio::test]
    async fn mock_backend_name_is_mock() {
        use std::sync::Arc as StdArc;
        let m = MockTradingBackend::new();
        assert_eq!(m.name(), "mock");
        // 通过 trait 调
        let backend: StdArc<dyn TradingBackend> = StdArc::new(m);
        assert_eq!(backend.name(), "mock");
    }

    #[tokio::test]
    async fn place_order_increments_id_and_stores() {
        let m = MockTradingBackend::new();
        assert_eq!(m.order_count(), 0);
        let ack1 = m.place_order(&mk_args()).await.unwrap();
        let ack2 = m.place_order(&mk_args()).await.unwrap();
        assert_eq!(ack1.order_id, "MOCK-1");
        assert_eq!(ack2.order_id, "MOCK-2");
        assert_eq!(m.order_count(), 2);
        assert_eq!(ack1.status.0, "Filled");
    }

    #[tokio::test]
    async fn failure_injector_place_order() {
        let m = with_failure_injector(
            MockTradingBackend::new(),
            FailureInjector::new().with_place_order_error("simulated outage"),
        );
        let e = m.place_order(&mk_args()).await.unwrap_err();
        assert!(matches!(e, TradingError::Backend(_)));
        assert_eq!(m.order_count(), 0); // 未入队
    }

    #[tokio::test]
    async fn failure_injector_get_balance() {
        let fi = FailureInjector {
            get_balance_error: Some("balance api down".into()),
            ..Default::default()
        };
        let m = with_failure_injector(MockTradingBackend::new(), fi);
        let e = m.get_balance().await.unwrap_err();
        assert!(matches!(e, TradingError::Backend(_)));
    }

    #[tokio::test]
    async fn default_balance_has_usdt_and_btc() {
        let m = MockTradingBackend::new();
        let b = m.get_balance().await.unwrap();
        assert_eq!(b.currencies.len(), 2);
        assert!(b.currencies.iter().any(|c| c.currency == "USDT"));
        assert!(b.currencies.iter().any(|c| c.currency == "BTC"));
    }

    #[tokio::test]
    async fn default_positions_has_btc() {
        let m = MockTradingBackend::new();
        let p = m.get_positions().await.unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].symbol, "BTC-USDT");
    }

    /// 买入 0.05 BTC @ 50_000:应扣 2500 USDT,BTC +0.05,持仓加权均价不变
    #[tokio::test]
    async fn buy_updates_balance_and_position() {
        let m = MockTradingBackend::new();
        let _ = m.place_order(&mk_args()).await.unwrap();

        let b = m.get_balance().await.unwrap();
        let usdt = b.currencies.iter().find(|c| c.currency == "USDT").unwrap();
        let btc = b.currencies.iter().find(|c| c.currency == "BTC").unwrap();
        assert!(
            (usdt.free - 7_500.0).abs() < 1e-6,
            "usdt free = {}",
            usdt.free
        );
        assert!((btc.free - 0.15).abs() < 1e-6, "btc free = {}", btc.free);

        let p = m.get_positions().await.unwrap();
        assert_eq!(p.len(), 1);
        assert!((p[0].quantity - 0.15).abs() < 1e-6);
        assert!((p[0].entry_price - 50_000.0).abs() < 1e-6);
    }

    /// 卖出 0.1 BTC @ 50_000:应增 5000 USDT,BTC 减至 0,持仓被平
    #[tokio::test]
    async fn sell_flatten_position_closes_row() {
        let m = MockTradingBackend::new();
        let args = PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Sell,
            quantity: 0.1,
            order_type: OrderKind::Limit,
            price: Some(50_000.0),
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        };
        m.place_order(&args).await.unwrap();

        let b = m.get_balance().await.unwrap();
        let usdt = b.currencies.iter().find(|c| c.currency == "USDT").unwrap();
        let btc = b.currencies.iter().find(|c| c.currency == "BTC").unwrap();
        assert!((usdt.free - 15_000.0).abs() < 1e-6);
        assert!(btc.free.abs() < 1e-9);

        let p = m.get_positions().await.unwrap();
        assert!(
            p.is_empty(),
            "positions should be empty after flatten, got {:?}",
            p
        );
    }

    /// 不存在的 symbol:开新仓并加入对应币种余额
    #[tokio::test]
    async fn unknown_symbol_opens_new_position() {
        let m = MockTradingBackend::new();
        let args = PlaceOrderArgs {
            symbol: "ETH-USDT".into(),
            side: OrderSide::Buy,
            quantity: 1.0,
            order_type: OrderKind::Limit,
            price: Some(3_000.0),
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        };
        m.place_order(&args).await.unwrap();

        let b = m.get_balance().await.unwrap();
        let usdt = b.currencies.iter().find(|c| c.currency == "USDT").unwrap();
        let eth = b.currencies.iter().find(|c| c.currency == "ETH").unwrap();
        assert!((usdt.free - 7_000.0).abs() < 1e-6);
        assert!((eth.free - 1.0).abs() < 1e-6);

        let p = m.get_positions().await.unwrap();
        let eth_pos = p.iter().find(|pos| pos.symbol == "ETH-USDT").unwrap();
        assert!((eth_pos.quantity - 1.0).abs() < 1e-6);
        assert!((eth_pos.entry_price - 3_000.0).abs() < 1e-6);
    }

    // ── Stage E:状态机字段初始化检查 ─────────────────────────

    /// 新建 Mock 时取消/改单/计数字段为初始值
    #[test]
    fn mock_default_cancelled_and_replaced_ids_empty() {
        let m = MockTradingBackend::new();
        assert!(m.cancelled_ids.lock().unwrap().is_empty());
        assert!(m.replaced_ids.lock().unwrap().is_empty());
        assert_eq!(*m.cancel_count.lock().unwrap(), 0);
    }

    /// cancel 后订单状态变为 Cancelled + 加入 cancelled_ids
    #[tokio::test]
    async fn cancel_marks_status_cancelled() {
        let m = MockTradingBackend::new();
        let ack = m.place_order(&mk_args()).await.unwrap();
        m.cancel_order(&ack.order_id).await.unwrap();

        let orders = m.orders.lock().unwrap();
        let cancelled = orders.iter().find(|o| o.order_id == ack.order_id).unwrap();
        assert_eq!(cancelled.status.0, "Cancelled");
        assert!(m.cancelled_ids.lock().unwrap().contains(&ack.order_id));
        assert_eq!(*m.cancel_count.lock().unwrap(), 1);
    }

    /// 同一 ID 二次 cancel 返回错误
    #[tokio::test]
    async fn cancel_duplicate_id_returns_error() {
        let m = MockTradingBackend::new();
        let ack = m.place_order(&mk_args()).await.unwrap();
        m.cancel_order(&ack.order_id).await.unwrap();
        let e = m.cancel_order(&ack.order_id).await.unwrap_err();
        assert!(matches!(e, TradingError::Backend(_)));
        assert!(format!("{}", e).contains("already cancelled"));
    }

    /// 不存在的 ID cancel 返回错误
    #[tokio::test]
    async fn cancel_unknown_id_returns_error() {
        let m = MockTradingBackend::new();
        let e = m.cancel_order("DOES-NOT-EXIST").await.unwrap_err();
        assert!(matches!(e, TradingError::Backend(_)));
        assert!(format!("{}", e).contains("not found"));
    }

    /// replace 更新价格/数量 + 加入 replaced_ids
    #[tokio::test]
    async fn replace_updates_price_quantity() {
        let m = MockTradingBackend::new();
        let ack = m.place_order(&mk_args()).await.unwrap();
        let new_args = PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.2,
            order_type: OrderKind::Limit,
            price: Some(51_000.0),
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        };
        let new_ack = m.replace_order(&ack.order_id, &new_args).await.unwrap();
        assert_eq!(new_ack.order_id, ack.order_id);
        assert_eq!(new_ack.quantity, 0.2);
        assert!(m.replaced_ids.lock().unwrap().contains(&ack.order_id));

        let orders = m.orders.lock().unwrap();
        let replaced = orders.iter().find(|o| o.order_id == ack.order_id).unwrap();
        assert_eq!(replaced.status.0, "Replaced");
    }
}
