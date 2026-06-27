//! 声明式 Agent
//!
//! 与 ReActAgent 的关键区别：
//! - ReActAgent: Observe → Think → Act(直接调用工具) → Verify
//! - DeclarativeAgent: Observe → Think → Act(返回 Intent) → Verify(Harness 裁决)
//!
//! Agent 只表达"想做什么"，由 Harness 层裁决后决定是否执行。

use axon_core::harness_types::{AgentIntent, HarnessResult, TaskContext};
use axon_harness::HarnessBridge;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::backend::LLMBackend;
use crate::types::Message;

/// 声明式 Agent 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclarativeAgentConfig {
    /// Agent ID，如 "market_agent"
    pub agent_id: String,
    /// 最大迭代次数，默认 5
    pub max_iterations: u32,
    /// 每轮最大 Token，默认 8000
    pub max_tokens_per_turn: u64,
    /// 最小置信度阈值，默认 0.3
    pub min_confidence: f64,
    /// Agent 系统提示词
    pub system_prompt: String,
}

impl Default for DeclarativeAgentConfig {
    fn default() -> Self {
        Self {
            agent_id: "agent".into(),
            max_iterations: 5,
            max_tokens_per_turn: 8000,
            min_confidence: 0.3,
            system_prompt: String::new(),
        }
    }
}

/// 声明式 Agent
///
/// Act 阶段返回 Intent，不直接调用工具。Harness 裁决后决定是否执行。
pub struct DeclarativeAgent {
    config: DeclarativeAgentConfig,
    backend: Box<dyn LLMBackend>,
    harness: HarnessBridge,
}

impl DeclarativeAgent {
    /// 创建声明式 Agent
    pub fn new(
        config: DeclarativeAgentConfig,
        backend: Box<dyn LLMBackend>,
        harness: HarnessBridge,
    ) -> Self {
        Self {
            config,
            backend,
            harness,
        }
    }

    /// 执行任务
    ///
    /// 流程：
    /// 1. 检查熔断器 → 如果熔断，返回 CircuitBreak
    /// 2. 循环 (最多 max_iterations 次):
    ///    a. 检查安全阀 (can_proceed)
    ///    b. OBSERVE + THINK: 调用 LLM 分析上下文并生成意图
    ///    c. ACT: 构造 AgentIntent（声明式）
    ///    d. VERIFY: 调用 harness.adjudicate()
    ///       - Approved → 执行（如果有关联工具）
    ///       - Rejected → 返回 Rejected
    ///       - NeedRevision → 反馈加入上下文，继续
    ///       - CircuitBreak → 返回 CircuitBreak
    pub async fn run(&mut self, task: &str) -> HarnessResult {
        // 1. 检查熔断器
        if self.harness.is_circuit_break() {
            return HarnessResult::CircuitBreak;
        }

        let mut ctx = TaskContext {
            step: 0,
            tokens_used: 0,
            task_description: task.to_string(),
            current_agent: self.config.agent_id.clone(),
            started_at: now_secs(),
            metadata: serde_json::Value::Null,
        };

        let mut messages = vec![
            Message::system(&self.config.system_prompt),
            Message::user(task),
        ];

        let mut revision_feedback = String::new();

        for iteration in 0..self.config.max_iterations {
            // 2a. 安全阀检查
            if !self.harness.can_proceed(&ctx) {
                return HarnessResult::MaxIterationsReached;
            }

            // 2b. OBSERVE + THINK: 调用 LLM
            if !revision_feedback.is_empty() {
                messages.push(Message::user(format!(
                    "请根据以下反馈修改你的计划：{revision_feedback}"
                )));
                revision_feedback.clear();
            }

            let response = match self.backend.complete(&messages).await {
                Ok(r) => r,
                Err(e) => {
                    debug!("LLM 调用失败: {e}");
                    return HarnessResult::Rejected {
                        intent: self.make_intent("", None, serde_json::Value::Null, 0.0, &e.to_string()),
                        reason: format!("LLM 调用失败: {e}"),
                    };
                }
            };

            // 估算 Token 消耗
            let tokens = response.token_usage.total_tokens as u64;
            ctx.advance(tokens);
            let _zone = self.harness.consume_tokens(tokens, "default");

            // 解析 LLM 输出为 Intent
            let content = response.content.unwrap_or_default();
            let intent = self.parse_intent(&content, tokens);

            // 置信度检查
            if intent.confidence < self.config.min_confidence {
                let confidence = intent.confidence;
                let threshold = self.config.min_confidence;
                return HarnessResult::Rejected {
                    intent,
                    reason: format!("置信度 {confidence} 低于阈值 {threshold}"),
                };
            }

            // 2c. ACT: 构造 AgentIntent（已在上面完成）

            // 2d. VERIFY: Harness 裁决
            let adjudication = self.harness.adjudicate(&intent, &ctx);
            debug!(
                agent = %self.config.agent_id,
                iteration,
                action = %intent.action,
                ?adjudication,
                "Harness 裁决结果"
            );

            match adjudication {
                axon_harness::Adjudication::Approved => {
                    // 检查工具门控
                    if let Some(tool) = &intent.tool {
                        let gate = self.harness.check_tool(
                            tool,
                            &self.config.agent_id,
                            &intent.params,
                        );
                        match gate {
                            axon_harness::GateResult::Allowed => {
                                self.harness.record_tool_call(
                                    tool,
                                    &self.config.agent_id,
                                    &intent.params,
                                    "executed",
                                );
                                return HarnessResult::Success {
                                    intent,
                                    tool_result: "executed".into(),
                                    iterations: iteration + 1,
                                    tokens_used: ctx.tokens_used,
                                };
                            }
                            axon_harness::GateResult::Denied(reason) => {
                                return HarnessResult::ToolDenied { intent, reason };
                            }
                            axon_harness::GateResult::NeedsApproval => {
                                return HarnessResult::NeedsApproval { intent };
                            }
                        }
                    } else {
                        // 无工具调用，IntentOnly
                        return HarnessResult::IntentOnly {
                            intent,
                            iterations: iteration + 1,
                        };
                    }
                }
                axon_harness::Adjudication::Rejected(reason) => {
                    return HarnessResult::Rejected { intent, reason };
                }
                axon_harness::Adjudication::NeedRevision(feedback) => {
                    revision_feedback = feedback;
                    messages.push(Message::assistant(&intent.action));
                }
                axon_harness::Adjudication::CircuitBreak => {
                    return HarnessResult::CircuitBreak;
                }
            }
        }

        HarnessResult::MaxIterationsReached
    }

    fn make_intent(
        &self,
        action: &str,
        tool: Option<String>,
        params: serde_json::Value,
        confidence: f64,
        reasoning: &str,
    ) -> AgentIntent {
        AgentIntent {
            action: action.to_string(),
            tool,
            params,
            confidence,
            reasoning: reasoning.to_string(),
            estimated_tokens: self.config.max_tokens_per_turn,
        }
    }

    fn parse_intent(&self, content: &str, tokens: u64) -> AgentIntent {
        // 尝试从 LLM 输出解析结构化 Intent
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) {
            let action = parsed
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or(content)
                .to_string();
            let tool = parsed
                .get("tool")
                .and_then(|v| v.as_str())
                .map(String::from);
            let params = parsed
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let confidence = parsed
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5);
            let reasoning = parsed
                .get("reasoning")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            AgentIntent {
                action,
                tool,
                params,
                confidence,
                reasoning,
                estimated_tokens: tokens,
            }
        } else {
            // 非 JSON 输出，构造简单 Intent
            AgentIntent {
                action: content.to_string(),
                tool: None,
                params: serde_json::Value::Null,
                confidence: 0.5,
                reasoning: content.to_string(),
                estimated_tokens: tokens,
            }
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let cfg = DeclarativeAgentConfig::default();
        assert_eq!(cfg.agent_id, "agent");
        assert_eq!(cfg.max_iterations, 5);
        assert_eq!(cfg.min_confidence, 0.3);
    }

    #[test]
    fn test_parse_intent_json() {
        // 需要一个 mock backend，这里测试 parse_intent 逻辑
        let cfg = DeclarativeAgentConfig::default();
        // 用一个简单的 mock 来测试 parse_intent
        struct MockBackend;
        #[async_trait::async_trait]
        impl LLMBackend for MockBackend {
            async fn complete(&self, _messages: &[Message]) -> Result<crate::types::LLMResponse, crate::backend::LLMError> {
                unimplemented!()
            }
            async fn complete_with_tools(&self, _messages: &[Message], _tools: &[crate::backend::ToolDefinition]) -> Result<crate::types::LLMResponse, crate::backend::LLMError> {
                unimplemented!()
            }
            fn context_window_size(&self) -> usize {
                4096
            }
        }

        let agent = DeclarativeAgent::new(cfg, Box::new(MockBackend), HarnessBridge::none());

        let json_content = r#"{"action": "buy BTC", "tool": "place_order", "params": {"symbol": "BTC"}, "confidence": 0.9, "reasoning": "bullish"}"#;
        let intent = agent.parse_intent(json_content, 100);
        assert_eq!(intent.action, "buy BTC");
        assert_eq!(intent.tool, Some("place_order".into()));
        assert!((intent.confidence - 0.9).abs() < f64::EPSILON);

        let plain_content = "just a text response";
        let intent = agent.parse_intent(plain_content, 50);
        assert_eq!(intent.action, "just a text response");
        assert_eq!(intent.tool, None);
    }
}
