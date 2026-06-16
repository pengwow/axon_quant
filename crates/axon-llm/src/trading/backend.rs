//! TradingBackend trait:工具与后端的解耦点
//!
//! 所有 LLM 交易工具都通过 `Arc<dyn TradingBackend>` 调用,具体后端可以是
//! 真实交易所 / OMS / 回测引擎 / Mock。使用方按需在自己的 crate 实现本 trait。

use async_trait::async_trait;
use thiserror::Error;

use crate::trading::types::{
    BalanceSnapshot, OrderAck, PlaceOrderArgs, PortfolioSnapshot, PositionSnapshot,
};

/// 交易后端错误
#[derive(Debug, Error)]
pub enum TradingError {
    /// 参数解析失败
    #[error("参数解析失败: {0}")]
    InvalidArguments(String),
    /// 后端调用失败
    #[error("后端调用失败: {0}")]
    Backend(String),
    /// 风控拒绝
    #[error("风控拒绝: {0}")]
    RiskRejected(String),
    /// 两次提交 token 不匹配
    #[error("两次提交 token 不匹配")]
    ConfirmTokenMismatch,
    /// 未找到待确认订单
    #[error("未找到待确认订单: {0}")]
    NoPendingOrder(String),
}

/// 交易后端抽象
///
/// 所有 LLM 交易工具都通过 `Arc<dyn TradingBackend>` 调用,具体后端可以是
/// 真实交易所 / OMS / 回测引擎 / Mock。使用方按需在自己的 crate 实现本 trait。
#[async_trait]
pub trait TradingBackend: Send + Sync {
    /// 下单
    async fn place_order(&self, req: &PlaceOrderArgs) -> Result<OrderAck, TradingError>;
    /// 查询余额
    async fn get_balance(&self) -> Result<BalanceSnapshot, TradingError>;
    /// 查询持仓
    async fn get_positions(&self) -> Result<Vec<PositionSnapshot>, TradingError>;
    /// 查询完整投资组合
    ///
    /// 默认采用 `tokio::try_join!` 并发拉取 balance + positions。
    /// 后端若有更高效实现(如单次 API 调用)可 override。
    async fn get_portfolio(&self) -> Result<PortfolioSnapshot, TradingError> {
        let (balance, positions) = tokio::try_join!(self.get_balance(), self.get_positions(),)?;
        Ok(PortfolioSnapshot { balance, positions })
    }
}

// ── 测试 ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::types::{OrderKind, OrderSide, OrderStatus, TimeInForce};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// 测试用最小实现:固定余额 + 固定持仓,验证 trait 默认 get_portfolio 工作
    struct TestBackend {
        balance: BalanceSnapshot,
        positions: Vec<PositionSnapshot>,
        place_calls: Arc<Mutex<Vec<PlaceOrderArgs>>>,
    }

    #[async_trait]
    impl TradingBackend for TestBackend {
        async fn place_order(&self, req: &PlaceOrderArgs) -> Result<OrderAck, TradingError> {
            self.place_calls.lock().expect("poisoned").push(req.clone());
            Ok(OrderAck {
                order_id: "TEST-1".into(),
                symbol: req.symbol.clone(),
                side: req.side,
                quantity: req.quantity,
                status: OrderStatus("Filled".into()),
                timestamp_ms: 0,
                confirm_token: None,
            })
        }
        async fn get_balance(&self) -> Result<BalanceSnapshot, TradingError> {
            Ok(self.balance.clone())
        }
        async fn get_positions(&self) -> Result<Vec<PositionSnapshot>, TradingError> {
            Ok(self.positions.clone())
        }
    }

    fn mk_args() -> PlaceOrderArgs {
        PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: OrderKind::Limit,
            price: Some(50_000.0),
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn default_get_portfolio_concurrent() {
        let backend = TestBackend {
            balance: BalanceSnapshot {
                currencies: vec![],
                as_of_ms: 0,
            },
            positions: vec![PositionSnapshot {
                symbol: "BTC-USDT".into(),
                quantity: 0.1,
                entry_price: 50_000.0,
                unrealized_pnl: 0.0,
                as_of_ms: 0,
            }],
            place_calls: Arc::new(Mutex::new(vec![])),
        };
        let snap = backend.get_portfolio().await.unwrap();
        assert_eq!(snap.positions.len(), 1);
        assert_eq!(snap.balance.currencies.len(), 0);
    }

    #[tokio::test]
    async fn place_order_returns_ack() {
        let backend = TestBackend {
            balance: BalanceSnapshot {
                currencies: vec![],
                as_of_ms: 0,
            },
            positions: vec![],
            place_calls: Arc::new(Mutex::new(vec![])),
        };
        let ack = backend.place_order(&mk_args()).await.unwrap();
        assert_eq!(ack.order_id, "TEST-1");
        assert_eq!(ack.status.0, "Filled");
    }
}
