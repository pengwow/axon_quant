//! FinishBarTool:LLM 结束当前 bar 工具
//!
//! 用于通知系统当前交易 bar 结束，返回 bar 期间的交易汇总信息。
//! 会查询后端获取实际持仓和余额,生成有意义的摘要。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::tools::{Tool, ToolError};
use crate::trading::backend::TradingBackend;

/// FinishBarTool 输入参数
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct FinishBarArgs {
    /// 可选的 bar 结束备注
    #[serde(default)]
    pub note: Option<String>,
}

/// Bar 结束汇总信息
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FinishBarResult {
    /// 当前时间戳(毫秒)
    pub timestamp_ms: i64,
    /// bar 结束标记
    pub finished: bool,
    /// 备注信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 摘要信息
    pub summary: String,
    /// 当前持仓数量
    pub position_count: usize,
    /// 当前现金余额
    pub cash_balance: Option<f64>,
}

/// FinishBar 工具
pub struct FinishBarTool {
    backend: Arc<dyn TradingBackend>,
}

impl FinishBarTool {
    /// 构造
    pub fn new(backend: Arc<dyn TradingBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for FinishBarTool {
    fn name(&self) -> &str {
        "finish_bar"
    }

    fn description(&self) -> &str {
        "结束当前交易 bar，返回 bar 期间交易汇总信息(持仓数、余额等)"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "note": {"type": "string", "description": "可选的 bar 结束备注"}
            }
        })
    }

    async fn execute(&self, arguments: &str) -> Result<String, ToolError> {
        let args: FinishBarArgs = serde_json::from_str(arguments).unwrap_or_default();

        // 查询后端获取实际状态
        let positions = self
            .backend
            .get_positions()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("查询持仓失败: {}", e)))?;

        let balance = self
            .backend
            .get_balance()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("查询余额失败: {}", e)))?;

        // 提取现金余额(取第一个货币,通常是 USDT)
        let cash_balance = balance.currencies.first().map(|c| c.free);

        let summary = format!(
            "bar finished: {} position(s), cash={}",
            positions.len(),
            cash_balance
                .map(|c| format!("{:.2}", c))
                .unwrap_or_else(|| "N/A".into())
        );

        let result = FinishBarResult {
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            finished: true,
            note: args.note,
            summary,
            position_count: positions.len(),
            cash_balance,
        };

        serde_json::to_string(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("序列化失败: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trading::mock::MockTradingBackend;

    #[tokio::test]
    async fn execute_returns_finished_result() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = FinishBarTool::new(m);
        let s = tool.execute("{}").await.unwrap();
        let result: FinishBarResult = serde_json::from_str(&s).unwrap();
        assert!(result.finished);
        assert!(result.summary.contains("bar finished"));
        assert!(result.summary.contains("position(s)"));
        assert!(result.cash_balance.is_some());
    }

    #[tokio::test]
    async fn execute_with_note_includes_note() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = FinishBarTool::new(m);
        let s = tool.execute(r#"{"note":"end of day"}"#).await.unwrap();
        let result: FinishBarResult = serde_json::from_str(&s).unwrap();
        assert_eq!(result.note, Some("end of day".to_string()));
    }

    #[tokio::test]
    async fn empty_json_treated_as_default() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = FinishBarTool::new(m);
        let s = tool.execute("").await.unwrap();
        let result: FinishBarResult = serde_json::from_str(&s).unwrap();
        assert!(result.finished);
        assert!(result.note.is_none());
    }

    #[tokio::test]
    async fn name_and_description_and_schema() {
        let m = Arc::new(MockTradingBackend::new());
        let tool = FinishBarTool::new(m);
        assert_eq!(tool.name(), "finish_bar");
        assert!(tool.description().contains("结束"));
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["note"].is_object());
    }
}
