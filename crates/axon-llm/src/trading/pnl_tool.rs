//! GetPnlTool:LLM 查询账户盈亏工具

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::tools::{Tool, ToolError};
use crate::trading::backend::TradingBackend;

/// GetPnlTool 输入参数
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct GetPnlArgs {
    /// 可选按 symbol 过滤
    #[serde(default)]
    pub symbol: Option<String>,
}

/// 单个持仓盈亏信息
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PositionPnl {
    /// 交易对
    pub symbol: String,
    /// 持仓数量
    pub quantity: f64,
    /// 开仓均价
    pub entry_price: f64,
    /// 浮动盈亏
    pub unrealized_pnl: f64,
    /// 浮动盈亏比例(%)
    pub unrealized_pnl_pct: f64,
}

/// 账户盈亏快照
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PnlSnapshot {
    /// 各持仓盈亏
    pub positions: Vec<PositionPnl>,
    /// 总浮动盈亏
    pub total_unrealized_pnl: f64,
    /// 快照时间戳(毫秒)
    pub as_of_ms: i64,
}

/// GetPnl 工具
pub struct GetPnlTool {
    backend: Arc<dyn TradingBackend>,
}

impl GetPnlTool {
    /// 构造
    pub fn new(backend: Arc<dyn TradingBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for GetPnlTool {
    fn name(&self) -> &str {
        "get_pnl"
    }

    fn description(&self) -> &str {
        "查询账户盈亏信息(浮动盈亏);可选按 symbol 过滤"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "symbol": {"type": "string", "description": "可选,按 symbol 过滤持仓"}
            }
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, ToolError> {
        let args: GetPnlArgs = serde_json::from_str(arguments).unwrap_or_default();

        let positions = self
            .backend
            .get_positions()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let mut filtered_positions = if let Some(sym) = args.symbol.as_deref() {
            positions.into_iter().filter(|p| p.symbol == sym).collect()
        } else {
            positions
        };

        let position_pnls: Vec<PositionPnl> = filtered_positions
            .into_iter()
            .map(|p| {
                let pnl_pct = if p.entry_price != 0.0 {
                    (p.unrealized_pnl / (p.entry_price * p.quantity.abs())) * 100.0
                } else {
                    0.0
                };
                PositionPnl {
                    symbol: p.symbol,
                    quantity: p.quantity,
                    entry_price: p.entry_price,
                    unrealized_pnl: p.unrealized_pnl,
                    unrealized_pnl_pct: pnl_pct,
                }
            })
            .collect();

        let total_unrealized_pnl = position_pnls.iter().map(|p| p.unrealized_pnl).sum::<f64>();

        let snapshot = PnlSnapshot {
            positions: position_pnls,
            total_unrealized_pnl,
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
    use crate::trading::mock::{FailureInjector, MockTradingBackend};

    fn mock_with_failure(m: MockTradingBackend, fi: FailureInjector) -> MockTradingBackend {
        *m.failure_injector.lock().expect("poisoned") = fi;
        m
    }

    #[tokio::test]
    async fn default_args_returns_all_positions_pnl() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = GetPnlTool::new(m);
        let s = tool.execute("{}").await.unwrap();
        let snap: PnlSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.positions.len(), 1);
        assert!(snap.total_unrealized_pnl != 0.0);
    }

    #[tokio::test]
    async fn symbol_filter_works() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = GetPnlTool::new(m);
        let s = tool.execute(r#"{"symbol":"BTC-USDT"}"#).await.unwrap();
        let snap: PnlSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.positions.len(), 1);
        assert_eq!(snap.positions[0].symbol, "BTC-USDT");
    }

    #[tokio::test]
    async fn symbol_filter_non_existent() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = GetPnlTool::new(m);
        let s = tool.execute(r#"{"symbol":"ETH-USDT"}"#).await.unwrap();
        let snap: PnlSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.positions.len(), 0);
        assert_eq!(snap.total_unrealized_pnl, 0.0);
    }

    #[tokio::test]
    async fn empty_json_treated_as_default() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = GetPnlTool::new(m);
        let s = tool.execute("").await.unwrap();
        let snap: PnlSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.positions.len(), 1);
    }

    #[tokio::test]
    async fn backend_error_propagates() {
        let fi = FailureInjector {
            get_positions_error: Some("positions api down".into()),
            ..Default::default()
        };
        let m = Arc::new(mock_with_failure(MockTradingBackend::new(), fi));
        let tool = GetPnlTool::new(m);
        let e = tool.execute("{}").await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
    }

    #[tokio::test]
    async fn name_and_schema() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = GetPnlTool::new(m);
        assert_eq!(tool.name(), "get_pnl");
        let schema = tool.parameters_schema();
        assert_eq!(schema["properties"]["symbol"]["type"], "string");
    }

    #[tokio::test]
    async fn pnl_calculation() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = GetPnlTool::new(m);
        let s = tool.execute("{}").await.unwrap();
        let snap: PnlSnapshot = serde_json::from_str(&s).unwrap();
        for pos in &snap.positions {
            if pos.entry_price != 0.0 {
                let expected_pct =
                    (pos.unrealized_pnl / (pos.entry_price * pos.quantity.abs())) * 100.0;
                assert!((pos.unrealized_pnl_pct - expected_pct).abs() < 0.01);
            }
        }
    }
}
