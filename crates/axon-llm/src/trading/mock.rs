//! Mock 后端:用于单测 / demo,与真实交易所完全隔离
//!
//! 使用方按需在自己 crate 实现 `TradingBackend` 适配真实交易所 / OMS / 回测引擎。

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
}

#[async_trait]
impl TradingBackend for MockTradingBackend {
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
}
