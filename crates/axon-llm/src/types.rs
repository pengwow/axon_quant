//! 核心数据类型：Message / Role / LLMResponse / TokenUsage / FinishReason / ToolCall

use serde::{Deserialize, Serialize};

// ─── 角色枚举 ──────────────────────────────────────────────

/// 聊天消息角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// 系统提示
    System,
    /// 用户
    User,
    /// 助手
    Assistant,
    /// 工具
    Tool,
}

impl Role {
    /// 序列化为 LLM 通用协议中的字符串形式（OpenAI / Anthropic 通用）
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

// ─── Token 使用统计 ─────────────────────────────────────────

/// Token 使用统计（prompt + completion = total）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// 提示 token 数
    pub prompt_tokens: usize,
    /// 完成 token 数
    pub completion_tokens: usize,
    /// 合计 token 数
    pub total_tokens: usize,
}

impl TokenUsage {
    /// 构造一个新的 TokenUsage，自动计算 total
    pub fn new(prompt_tokens: usize, completion_tokens: usize) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }
    }

    /// 累加另一次调用的使用量
    pub fn add(&mut self, other: TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

// ─── 完成原因 ──────────────────────────────────────────────

/// LLM 完成原因
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// 自然结束
    Stop,
    /// 达到长度上限
    Length,
    /// 触发了工具调用
    ToolCalls,
    /// 被内容过滤器拦截
    ContentFilter,
}

// ─── 工具调用 ──────────────────────────────────────────────

/// LLM 发起的工具调用
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// 调用 ID（用于关联 tool_result）
    pub id: String,
    /// 工具函数名
    pub function_name: String,
    /// 参数 JSON 字符串
    pub arguments: String,
}

// ─── 消息 ──────────────────────────────────────────────

/// 聊天消息
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// 角色
    pub role: Role,
    /// 文本内容
    pub content: String,
    /// 工具调用 ID（Tool 角色时必填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 助手发起的工具调用（Assistant 角色时可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl Message {
    /// 构造系统消息
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// 构造用户消息
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// 构造助手消息
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// 构造工具结果消息
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
        }
    }
}

// ─── LLM 响应 ──────────────────────────────────────────────

/// LLM 原始响应
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LLMResponse {
    /// 文本内容（可能是 None，若仅返回工具调用）
    pub content: Option<String>,
    /// 模型思考过程(DeepSeek-R1 / o系列 的 reasoning_content),非推理模型为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// 工具调用列表
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Token 使用统计
    pub token_usage: TokenUsage,
    /// 完成原因
    pub finish_reason: FinishReason,
}

impl LLMResponse {
    /// 构造纯文本响应
    pub fn text(content: impl Into<String>, usage: TokenUsage) -> Self {
        Self {
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: None,
            token_usage: usage,
            finish_reason: FinishReason::Stop,
        }
    }

    /// 构造工具调用响应
    pub fn tool_calls(calls: Vec<ToolCall>, usage: TokenUsage) -> Self {
        Self {
            content: None,
            reasoning_content: None,
            tool_calls: Some(calls),
            token_usage: usage,
            finish_reason: FinishReason::ToolCalls,
        }
    }

    /// 是否包含工具调用（非空列表）
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
    }
}
