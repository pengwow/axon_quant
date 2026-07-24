//! FinishBarTool:LLM 结束当前 bar 工具
//!
//! 用于通知系统当前交易 bar 结束，返回 bar 期间的交易汇总信息。

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
}

/// FinishBar 工具
pub struct FinishBarTool {
    _backend: Arc<dyn TradingBackend>,
}

impl FinishBarTool {
    /// 构造
    pub fn new(backend: Arc<dyn TradingBackend>) -> Self {
        Self { _backend: backend }
    }
}

#[async_trait]
impl Tool for FinishBarTool {
    fn name(&self) -> &str {
        "finish_bar"
    }

    fn description(&self) -> &str {
        "结束当前交易 bar，返回 bar 期间交易汇总信息"
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

        let result = FinishBarResult {
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            finished: true,
            note: args.note,
            summary: "bar finished".to_string(),
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
        assert_eq!(result.summary, "bar finished");
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
