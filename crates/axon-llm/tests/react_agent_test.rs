//! TDD 第七轮：ReActAgent 主循环
//!
//! 核心测试场景：
//! 1. 工具调用循环：LLM 返回 ToolCalls → 执行工具 → 反馈结果 → 继续
//! 2. 最终答案：LLM 返回文本（无工具调用）→ Agent 返回 AgentResponse
//! 3. 权限检查：未授权工具名 → 立即拒绝
//! 4. 最大轮次：达到上限 → 返回 MaxIterationsExceeded 错误
//! 5. Token 聚合：正确累加每次 LLM 调用的使用量

use std::sync::atomic::{AtomicBool, Ordering};

use axon_llm::ReActAgent;
use axon_llm::agent::{AgentConfig, AgentError, ErrorSeverity};
use axon_llm::tools::{Tool, ToolError};
use axon_llm::{LLMBackend, LLMResponse, Message, TokenUsage, ToolCall};

// ─── 测试工具：EchoTool ─────────────────────────────────────

struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "回显输入"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
    }
    async fn execute(&self, arguments: &str) -> Result<String, ToolError> {
        let v: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;
        let text = v["text"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments("缺少 text".into()))?;
        Ok(format!("echo: {}", text))
    }
}

// ─── Mock：返回纯文本（模拟无需工具的场景） ─────────────────────

struct TextOnlyBackend;

#[async_trait::async_trait]
impl LLMBackend for TextOnlyBackend {
    async fn complete(&self, _: &[Message]) -> Result<LLMResponse, axon_llm::backend::LLMError> {
        Ok(LLMResponse::text("最终答案", TokenUsage::new(5, 3)))
    }
    async fn complete_with_tools(
        &self,
        _: &[Message],
        _: &[axon_llm::backend::ToolDefinition],
    ) -> Result<LLMResponse, axon_llm::backend::LLMError> {
        Ok(LLMResponse::text("最终答案", TokenUsage::new(5, 3)))
    }
    fn context_window_size(&self) -> usize {
        4096
    }
}

// ─── Mock：触发一轮工具调用后返回文本 ──────────────────────────

struct OneToolThenTextBackend {
    called: AtomicBool,
}

impl OneToolThenTextBackend {
    fn new() -> Self {
        Self {
            called: AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl LLMBackend for OneToolThenTextBackend {
    async fn complete(&self, _: &[Message]) -> Result<LLMResponse, axon_llm::backend::LLMError> {
        unreachable!("只使用 complete_with_tools")
    }
    async fn complete_with_tools(
        &self,
        _: &[Message],
        _: &[axon_llm::backend::ToolDefinition],
    ) -> Result<LLMResponse, axon_llm::backend::LLMError> {
        // 第一轮：工具调用；第二轮：最终答案
        if !self.called.swap(true, Ordering::SeqCst) {
            Ok(LLMResponse::tool_calls(
                vec![ToolCall {
                    id: "c1".into(),
                    function_name: "echo".into(),
                    arguments: r#"{"text":"hello"}"#.into(),
                }],
                TokenUsage::new(10, 5),
            ))
        } else {
            Ok(LLMResponse::text("工具执行成功", TokenUsage::new(8, 2)))
        }
    }
    fn context_window_size(&self) -> usize {
        4096
    }
}

// ─── Mock：始终返回工具调用（触发最大轮次限制） ────────────────────

struct InfiniteToolBackend;

#[async_trait::async_trait]
impl LLMBackend for InfiniteToolBackend {
    async fn complete(&self, _: &[Message]) -> Result<LLMResponse, axon_llm::backend::LLMError> {
        unreachable!("只使用 complete_with_tools")
    }
    async fn complete_with_tools(
        &self,
        _: &[Message],
        _: &[axon_llm::backend::ToolDefinition],
    ) -> Result<LLMResponse, axon_llm::backend::LLMError> {
        Ok(LLMResponse::tool_calls(
            vec![ToolCall {
                id: "x".into(),
                function_name: "echo".into(),
                arguments: r#"{"text":"x"}"#.into(),
            }],
            TokenUsage::new(5, 2),
        ))
    }
    fn context_window_size(&self) -> usize {
        4096
    }
}

// ─── Mock：返回未授权工具 ────────────────────────────────────

struct UnauthorizedToolBackend;

#[async_trait::async_trait]
impl LLMBackend for UnauthorizedToolBackend {
    async fn complete(&self, _: &[Message]) -> Result<LLMResponse, axon_llm::backend::LLMError> {
        unreachable!("只使用 complete_with_tools")
    }
    async fn complete_with_tools(
        &self,
        _: &[Message],
        _: &[axon_llm::backend::ToolDefinition],
    ) -> Result<LLMResponse, axon_llm::backend::LLMError> {
        Ok(LLMResponse::tool_calls(
            vec![ToolCall {
                id: "x".into(),
                function_name: "dangerous_tool".into(),
                arguments: "{}".into(),
            }],
            TokenUsage::new(5, 2),
        ))
    }
    fn context_window_size(&self) -> usize {
        4096
    }
}

// ─── 测试：最终答案路径 ──────────────────────────────────────

#[tokio::test]
async fn test_agent_returns_final_answer_when_llm_returns_text() {
    let mut agent = ReActAgent::new(Box::new(TextOnlyBackend), AgentConfig::default());
    agent.add_tool(Box::new(EchoTool));

    let resp = agent.reason("你好").await.unwrap();
    assert_eq!(resp.answer, "最终答案");
    assert!(resp.iterations >= 1);
    assert_eq!(resp.reasoning_trace.len(), 1);
}

/// Token 使用量必须正确累加
#[tokio::test]
async fn test_agent_accumulates_token_usage() {
    let mut agent = ReActAgent::new(Box::new(TextOnlyBackend), AgentConfig::default());
    agent.add_tool(Box::new(EchoTool));

    let resp = agent.reason("hi").await.unwrap();
    // 5 + 3 = 8
    assert_eq!(resp.token_usage.total_tokens, 8);
    assert_eq!(resp.token_usage.prompt_tokens, 5);
    assert_eq!(resp.token_usage.completion_tokens, 3);
}

// ─── 测试：工具调用循环 ──────────────────────────────────────

#[tokio::test]
async fn test_agent_executes_tool_and_continues() {
    let mut agent = ReActAgent::new(
        Box::new(OneToolThenTextBackend::new()),
        AgentConfig::default(),
    );
    agent.add_tool(Box::new(EchoTool));

    let resp = agent.reason("分析").await.unwrap();
    assert_eq!(resp.answer, "工具执行成功");
    assert_eq!(resp.iterations, 2, "应经历 2 轮：工具调用 + 最终答案");
}

/// 工具执行结果必须出现在推理跟踪中
#[tokio::test]
async fn test_agent_traces_tool_observation() {
    let mut agent = ReActAgent::new(
        Box::new(OneToolThenTextBackend::new()),
        AgentConfig::default(),
    );
    agent.add_tool(Box::new(EchoTool));

    let resp = agent.reason("分析").await.unwrap();
    // 第一步应有 action 和 observation
    let first_step = &resp.reasoning_trace[0];
    assert!(first_step.action.is_some(), "第一步应有工具调用");
    assert!(first_step.observation.is_some(), "第一步应有工具返回结果");
    assert!(first_step.observation.as_ref().unwrap().contains("echo"));
}

// ─── 测试：权限拒绝 ─────────────────────────────────────────

#[tokio::test]
async fn test_agent_rejects_unauthorized_tool() {
    let config = AgentConfig::new().with_allowed_tools(vec!["echo".into()]); // 只允许 echo
    let mut agent = ReActAgent::new(Box::new(UnauthorizedToolBackend), config);
    agent.add_tool(Box::new(EchoTool));

    let err = agent.reason("执行危险操作").await.unwrap_err();
    assert!(matches!(err, AgentError::PermissionDenied(_)));
    assert_eq!(err.severity(), ErrorSeverity::Critical);
    assert!(!err.is_recoverable());
}

// ─── 测试：最大轮次限制 ─────────────────────────────────────

#[tokio::test]
async fn test_agent_respects_max_iterations() {
    let config = AgentConfig::new().with_max_iterations(3);
    let mut agent = ReActAgent::new(Box::new(InfiniteToolBackend), config);
    agent.add_tool(Box::new(EchoTool));

    let err = agent.reason("分析").await.unwrap_err();
    assert!(matches!(err, AgentError::MaxIterationsExceeded { max: 3 }));
}

// ─── 测试：工具不存在 ───────────────────────────────────────

#[tokio::test]
async fn test_agent_returns_error_when_tool_not_found() {
    let mut agent = ReActAgent::new(Box::new(InfiniteToolBackend), AgentConfig::default());
    // 注意：不注册任何工具

    let err = agent.reason("分析").await.unwrap_err();
    assert!(matches!(err, AgentError::ToolError(_)));
    assert!(err.is_recoverable());
}

// ─── 测试：with_max_memory ───────────────────────────────────

#[tokio::test]
async fn test_agent_with_max_memory() {
    let config = AgentConfig::new().with_max_iterations(1);
    let mut agent = ReActAgent::new(Box::new(TextOnlyBackend), config).with_max_memory(5);

    assert_eq!(agent.memory().len(), 0);

    for i in 0..10 {
        let _ = agent.reason(&format!("问题 {}", i)).await;
    }

    assert_eq!(agent.memory().len(), 5);
}

#[tokio::test]
async fn test_agent_with_max_memory_zero() {
    let config = AgentConfig::new().with_max_iterations(1);
    let mut agent = ReActAgent::new(Box::new(TextOnlyBackend), config).with_max_memory(0);

    let _ = agent.reason("问题").await;

    assert_eq!(agent.memory().len(), 0);
}

#[tokio::test]
async fn test_agent_with_max_memory_chain() {
    let config = AgentConfig::new().with_max_iterations(1);
    let mut agent = ReActAgent::new(Box::new(TextOnlyBackend), config).with_max_memory(3);

    agent.add_tool(Box::new(EchoTool));

    for i in 0..5 {
        let _ = agent.reason(&format!("问题 {}", i)).await;
    }

    assert_eq!(agent.memory().len(), 3);
}
