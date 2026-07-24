//! GetBookSnapshotTool:LLM 查询订单簿快照工具
//!
//! 使用 `L1MatchingEngine::depth(n)` 获取买卖盘各 n 档深度数据。

#![cfg(feature = "trading-backtest")]

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use axon_backtest::matching::{L1MatchingEngine, MatchingEngine};
use axon_core::types::Symbol;

use crate::tools::{Tool, ToolError};

/// GetBookSnapshotTool 输入参数
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct GetBookSnapshotArgs {
    /// 交易对,例 "BTC-USDT"
    #[serde(default)]
    pub symbol: Option<String>,
    /// 返回深度档数,默认 10
    #[serde(default = "default_levels")]
    pub levels: usize,
}

fn default_levels() -> usize {
    10
}

/// 订单簿档位数据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderBookLevel {
    /// 价格
    pub price: f64,
    /// 数量
    pub quantity: f64,
}

/// 订单簿快照
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderBookSnapshot {
    /// 交易对
    pub symbol: String,
    /// 买盘深度
    pub bids: Vec<OrderBookLevel>,
    /// 卖盘深度
    pub asks: Vec<OrderBookLevel>,
    /// 快照时间戳(毫秒)
    pub as_of_ms: i64,
}

/// 订单簿数据源 trait
#[async_trait]
pub trait OrderBookProvider: Send + Sync {
    async fn depth(&self, levels: usize) -> Result<(Vec<(f64, f64)>, Vec<(f64, f64)>), ToolError>;
}

#[async_trait]
impl OrderBookProvider for L1MatchingEngine {
    async fn depth(&self, levels: usize) -> Result<(Vec<(f64, f64)>, Vec<(f64, f64)>), ToolError> {
        let (bids, asks) = MatchingEngine::depth(self, levels);
        let bids: Vec<(f64, f64)> = bids.iter().map(|l| (l.price.as_f64(), l.quantity.as_f64())).collect();
        let asks: Vec<(f64, f64)> = asks.iter().map(|l| (l.price.as_f64(), l.quantity.as_f64())).collect();
        Ok((bids, asks))
    }
}

/// GetBookSnapshot 工具
pub struct GetBookSnapshotTool {
    provider: Arc<dyn OrderBookProvider>,
    symbol: Symbol,
}

impl GetBookSnapshotTool {
    /// 构造
    pub fn new(provider: Arc<dyn OrderBookProvider>, symbol: impl Into<Symbol>) -> Self {
        Self {
            provider,
            symbol: symbol.into(),
        }
    }

    /// 获取交易对
    pub fn symbol(&self) -> &str {
        self.symbol.as_str()
    }
}

#[async_trait]
impl Tool for GetBookSnapshotTool {
    fn name(&self) -> &str {
        "get_book_snapshot"
    }

    fn description(&self) -> &str {
        "查询订单簿快照(买卖盘深度);支持按 symbol 和 levels 过滤"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "symbol": {"type": "string", "description": "交易对,例 'BTC-USDT'"},
                "levels": {"type": "integer", "description": "返回深度档数,默认 10"}
            }
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, ToolError> {
        let args: GetBookSnapshotArgs = serde_json::from_str(arguments).unwrap_or_default();
        let levels = if args.levels == 0 { 10 } else { args.levels };

        let (bids, asks) = self.provider.depth(levels).await?;

        let snapshot_bids: Vec<OrderBookLevel> = bids
            .iter()
            .map(|(price, quantity)| OrderBookLevel { price: *price, quantity: *quantity })
            .collect();

        let snapshot_asks: Vec<OrderBookLevel> = asks
            .iter()
            .map(|(price, quantity)| OrderBookLevel { price: *price, quantity: *quantity })
            .collect();

        let snapshot = OrderBookSnapshot {
            symbol: self.symbol.to_string(),
            bids: snapshot_bids,
            asks: snapshot_asks,
            as_of_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        };

        serde_json::to_string(&snapshot)
            .map_err(|e| ToolError::ExecutionFailed(format!("序列化失败: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_backtest::matching::L1MatchingEngine;
    use axon_core::market::Side;
    use axon_core::order::{Order, OrderType, TimeInForce};
    use axon_core::types::{Price, Quantity, Symbol};

    fn make_limit_order(id: u64, side: Side, price: f64, qty: f64) -> Order {
        Order::spot(
            id,
            "BTC",
            "USDT",
            side,
            OrderType::Limit { price: Price::from_f64(price) },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        )
    }

    fn make_engine() -> L1MatchingEngine {
        let mut engine = L1MatchingEngine::with_symbol(Symbol::from("BTC-USDT"));
        engine.submit(make_limit_order(1, Side::Sell, 101.0, 1.0));
        engine.submit(make_limit_order(2, Side::Sell, 102.0, 2.0));
        engine.submit(make_limit_order(3, Side::Buy, 99.0, 1.0));
        engine.submit(make_limit_order(4, Side::Buy, 98.0, 2.0));
        engine
    }

    #[tokio::test]
    async fn default_args_returns_full_depth() {
        let engine = Arc::new(make_engine());
        let tool = GetBookSnapshotTool::new(engine, "BTC-USDT");
        let s = tool.execute("{}").await.unwrap();
        let snap: OrderBookSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.symbol, "BTC-USDT");
        assert_eq!(snap.bids.len(), 2);
        assert_eq!(snap.asks.len(), 2);
    }

    #[tokio::test]
    async fn custom_levels_limits_depth() {
        let engine = Arc::new(make_engine());
        let tool = GetBookSnapshotTool::new(engine, "BTC-USDT");
        let s = tool.execute(r#"{"levels": 1}"#).await.unwrap();
        let snap: OrderBookSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.bids.len(), 1);
        assert_eq!(snap.asks.len(), 1);
    }

    #[tokio::test]
    async fn empty_engine_returns_empty_snapshot() {
        let engine = Arc::new(L1MatchingEngine::with_symbol(Symbol::from("BTC-USDT")));
        let tool = GetBookSnapshotTool::new(engine, "BTC-USDT");
        let s = tool.execute("{}").await.unwrap();
        let snap: OrderBookSnapshot = serde_json::from_str(&s).unwrap();
        assert!(snap.bids.is_empty());
        assert!(snap.asks.is_empty());
    }

    #[tokio::test]
    async fn name_and_schema() {
        let engine = Arc::new(make_engine());
        let tool = GetBookSnapshotTool::new(engine, "BTC-USDT");
        assert_eq!(tool.name(), "get_book_snapshot");
        let schema = tool.parameters_schema();
        assert_eq!(schema["properties"]["symbol"]["type"], "string");
        assert_eq!(schema["properties"]["levels"]["type"], "integer");
    }
}
