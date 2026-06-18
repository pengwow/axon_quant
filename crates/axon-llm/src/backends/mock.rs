//! 测试用 Mock LLM Backend
//!
//! 提供:
//! - [`MockBackend::text_only`] — 固定返回一段文本(简化测试,常用)
//! - [`MockBackend::with_responses`] — 预编程一组响应(按调用顺序消费)
//! - [`MockBackend::exhausted`] — 显式标记耗尽
//!
//! ## 设计要点
//!
//! - **线程安全**:内部 `Mutex<Vec<LLMResponse>>` 包裹,多线程测试也可并发
//! - **工具调用支持**:每个 LLMResponse 都可能包含 tool_calls,与 `LLMBackend` 协议一致
//! - **fail loud**:响应耗尽返回 `LLMError::MockExhausted`,而非 panic,
//!   让上层 Agent 决定是终止循环还是改写测试

use std::sync::Mutex;

use async_trait::async_trait;

use crate::backend::{LLMBackend, LLMError, ToolDefinition};
use crate::types::{LLMResponse, Message, TokenUsage};

/// 预编程响应序列的 Mock Backend
pub struct MockBackend {
    responses: Mutex<Vec<LLMResponse>>,
}

impl MockBackend {
    /// 构造空 backend(无响应,首次调用即报 `MockExhausted`)
    pub fn new() -> Self {
        Self {
            responses: Mutex::new(Vec::new()),
        }
    }

    /// 固定返回纯文本(常见于"我只关心 ReAct 主循环通不通"场景)
    pub fn text_only(text: impl Into<String>) -> Self {
        let resp = LLMResponse::text(text, TokenUsage::new(0, 0));
        Self {
            responses: Mutex::new(vec![resp]),
        }
    }

    /// 预编程一组响应(按调用顺序消费)
    pub fn with_responses(responses: Vec<LLMResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }

    /// 追加一条响应(链式)
    pub fn push(&self, resp: LLMResponse) {
        self.responses.lock().expect("mock poisoned").push(resp);
    }

    /// 剩余响应数(测试中检查消费进度)
    pub fn remaining(&self) -> usize {
        self.responses.lock().expect("mock poisoned").len()
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LLMBackend for MockBackend {
    async fn complete(&self, _messages: &[Message]) -> Result<LLMResponse, LLMError> {
        let mut g = self.responses.lock().expect("mock poisoned");
        g.pop_front().ok_or(LLMError::MockExhausted)
    }

    async fn complete_with_tools(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse, LLMError> {
        // 与 complete 共用响应队列
        let mut g = self.responses.lock().expect("mock poisoned");
        g.pop_front().ok_or(LLMError::MockExhausted)
    }

    fn context_window_size(&self) -> usize {
        8192
    }
}

// 辅助扩展:为 Mutex<Vec<T>> 提供 pop_front
trait VecPopFront<T> {
    fn pop_front(&mut self) -> Option<T>;
}

impl<T> VecPopFront<T> for Vec<T> {
    fn pop_front(&mut self) -> Option<T> {
        if self.is_empty() {
            None
        } else {
            Some(self.remove(0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FinishReason, TokenUsage, ToolCall};

    #[tokio::test]
    async fn text_only_returns_text() {
        let b = MockBackend::text_only("hello");
        let resp = b.complete(&[]).await.expect("ok");
        assert_eq!(resp.content.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn exhausted_returns_error() {
        let b = MockBackend::new();
        let err = b.complete(&[]).await.expect_err("should fail");
        assert!(matches!(err, LLMError::MockExhausted));
    }

    #[tokio::test]
    async fn with_responses_consumed_in_order() {
        let v = vec![
            LLMResponse::text("a", TokenUsage::default()),
            LLMResponse::tool_calls(
                vec![ToolCall {
                    id: "1".into(),
                    function_name: "x".into(),
                    arguments: "{}".into(),
                }],
                TokenUsage::default(),
            ),
            LLMResponse {
                content: Some("c".into()),
                tool_calls: None,
                token_usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
            },
        ];
        let b = MockBackend::with_responses(v);
        assert_eq!(b.remaining(), 3);
        let r1 = b.complete(&[]).await.unwrap();
        assert_eq!(r1.content.as_deref(), Some("a"));
        let r2 = b.complete(&[]).await.unwrap();
        assert!(r2.has_tool_calls());
        let r3 = b.complete(&[]).await.unwrap();
        assert_eq!(r3.content.as_deref(), Some("c"));
        assert_eq!(b.remaining(), 0);
    }
}
