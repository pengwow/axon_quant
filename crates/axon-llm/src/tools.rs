//! 工具系统：Tool trait / ToolError / ToolResult
//!
//! 所有 LLM 可调用的功能单元都实现 [`Tool`] trait。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::backend::ToolDefinition;

/// 工具执行结果
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    /// 关联的 tool_call ID
    pub tool_call_id: String,
    /// 执行内容（字符串，约定为 JSON）
    pub content: String,
    /// 是否执行成功
    pub success: bool,
}

impl ToolResult {
    /// 构造成功结果
    pub fn ok(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            success: true,
        }
    }

    /// 构造失败结果
    pub fn err(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            success: false,
        }
    }
}

/// 工具执行错误
#[derive(Debug, Error, Clone)]
pub enum ToolError {
    /// 参数解析失败（JSON 不合法或缺字段）
    #[error("参数解析失败: {0}")]
    InvalidArguments(String),

    /// 工具执行失败（运行时异常）
    #[error("工具执行失败: {0}")]
    ExecutionFailed(String),

    /// 权限不足
    #[error("权限不足: {tool} 不允许执行 {operation}")]
    PermissionDenied {
        /// 工具名
        tool: String,
        /// 操作名
        operation: String,
    },
}

/// 工具 trait
///
/// 所有 LLM 可调用的功能单元都实现该 trait。Agent 内部通过
/// `HashMap<String, Box<dyn Tool>>` 索引。
#[async_trait]
pub trait Tool: Send + Sync {
    /// 工具名称（必须唯一，与 ToolDefinition.name 一致）
    fn name(&self) -> &str;

    /// 工具描述（给 LLM 阅读）
    fn description(&self) -> &str;

    /// 参数 JSON Schema
    fn parameters_schema(&self) -> serde_json::Value;

    /// 执行工具，arguments 为 JSON 字符串
    async fn execute(&self, arguments: &str) -> Result<String, ToolError>;

    /// 工具定义（组合 name + description + schema，默认实现）
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}
