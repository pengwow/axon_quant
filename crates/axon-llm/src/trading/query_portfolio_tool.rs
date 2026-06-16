//! QueryPortfolioTool:LLM 查询投资组合工具(balance + positions)

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::tools::{Tool, ToolError};
use crate::trading::backend::TradingBackend;
use crate::trading::types::QueryPortfolioArgs;

/// Query portfolio 工具
pub struct QueryPortfolioTool {
    /// 交易后端
    backend: Arc<dyn TradingBackend>,
}

impl QueryPortfolioTool {
    /// 构造
    pub fn new(backend: Arc<dyn TradingBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for QueryPortfolioTool {
    fn name(&self) -> &str {
        "query_portfolio"
    }

    fn description(&self) -> &str {
        "查询投资组合(余额 + 持仓);可选按 symbol 过滤持仓"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "symbol": {"type": "string", "description": "可选,按 symbol 过滤持仓(不影响 balance)"}
            }
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, ToolError> {
        // 缺字段 / 空 JSON / 解析失败 → 默认 args(全量返回)
        let args: QueryPortfolioArgs = serde_json::from_str(arguments).unwrap_or_default();

        let mut snapshot = self
            .backend
            .get_portfolio()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        if let Some(sym) = args.symbol.as_deref() {
            snapshot.positions.retain(|p| p.symbol == sym);
        }

        serde_json::to_string(&snapshot)
            .map_err(|e| ToolError::ExecutionFailed(format!("序列化失败: {}", e)))
    }
}

// ── 测试 ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::mock::{FailureInjector, MockTradingBackend};
    use crate::trading::types::PortfolioSnapshot;

    /// 辅助构造器:替换 mock 后端的 failure_injector
    fn mock_with_failure(m: MockTradingBackend, fi: FailureInjector) -> MockTradingBackend {
        *m.failure_injector.lock().expect("poisoned") = fi;
        m
    }

    #[tokio::test]
    async fn default_args_returns_full_portfolio() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = QueryPortfolioTool::new(m);
        let s = tool.execute("{}").await.unwrap();
        let snap: PortfolioSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.balance.currencies.len(), 2);
        assert_eq!(snap.positions.len(), 1);
    }

    #[tokio::test]
    async fn symbol_filter_only_affects_positions() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = QueryPortfolioTool::new(m);
        // 过滤一个不存在的 symbol
        let s = tool.execute(r#"{"symbol":"ETH-USDT"}"#).await.unwrap();
        let snap: PortfolioSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.positions.len(), 0); // 被过滤
        assert_eq!(snap.balance.currencies.len(), 2); // balance 不受影响
    }

    #[tokio::test]
    async fn symbol_filter_existing() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = QueryPortfolioTool::new(m);
        let s = tool.execute(r#"{"symbol":"BTC-USDT"}"#).await.unwrap();
        let snap: PortfolioSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.positions.len(), 1);
        assert_eq!(snap.positions[0].symbol, "BTC-USDT");
    }

    #[tokio::test]
    async fn empty_json_string_treated_as_default() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = QueryPortfolioTool::new(m);
        // 空字符串 → 解析失败 → 用 default
        let s = tool.execute("").await.unwrap();
        let snap: PortfolioSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.balance.currencies.len(), 2);
    }

    #[tokio::test]
    async fn invalid_json_treated_as_default() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = QueryPortfolioTool::new(m);
        let s = tool.execute("not a json").await.unwrap();
        let snap: PortfolioSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap.balance.currencies.len(), 2);
    }

    #[tokio::test]
    async fn backend_error_propagates() {
        let fi = FailureInjector {
            get_balance_error: Some("balance api down".into()),
            ..Default::default()
        };
        let m = Arc::new(mock_with_failure(MockTradingBackend::new(), fi));
        let tool = QueryPortfolioTool::new(m);
        let e = tool.execute("{}").await.unwrap_err();
        assert!(matches!(e, ToolError::ExecutionFailed(_)));
    }

    #[tokio::test]
    async fn name_and_schema() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = QueryPortfolioTool::new(m);
        assert_eq!(tool.name(), "query_portfolio");
        let schema = tool.parameters_schema();
        assert_eq!(schema["properties"]["symbol"]["type"], "string");
    }
}
