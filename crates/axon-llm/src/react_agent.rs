//! ReAct Agent 实现
//!
//! ReAct = Reasoning + Acting。Agent 通过 Thought → Action → Observation 循环
//! 与外部工具交互，直到 LLM 返回最终答案或达到最大轮次。

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agent::{AgentConfig, AgentError};
use crate::backend::{LLMBackend, ToolDefinition};
use crate::context::ContextManager;
use crate::tools::Tool;
use crate::types::{Message, Role, TokenUsage, ToolCall};

/// ReAct 推理步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningStep {
    /// 步骤序号
    pub step: usize,
    /// LLM 的思考内容（Thought）
    pub thought: String,
    /// 工具调用（Action）
    pub action: Option<ToolCall>,
    /// 工具执行结果（Observation）
    pub observation: Option<String>,
}

/// Agent 响应
#[derive(Debug, Clone)]
pub struct AgentResponse {
    /// 最终答案
    pub answer: String,
    /// 完整推理链
    pub reasoning_trace: Vec<ReasoningStep>,
    /// Token 使用统计
    pub token_usage: TokenUsage,
    /// 实际迭代轮次
    pub iterations: usize,
}

/// ReAct Agent
pub struct ReActAgent {
    /// LLM 后端
    backend: Box<dyn LLMBackend>,
    /// 已注册工具
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Agent 配置
    config: AgentConfig,
    /// 对话历史
    memory: crate::context::ConversationMemory,

    // explain feature（可选）
    /// 异步记录器（fire-and-forget）
    #[cfg(feature = "explain")]
    recorder: Option<crate::explain::DecisionRecorder>,
    /// 解释存储（外部可读）
    #[cfg(feature = "explain")]
    explanation_store: Option<Arc<crate::explain::ExplanationStore>>,
}

impl ReActAgent {
    /// 创建新的 ReActAgent
    pub fn new(backend: Box<dyn LLMBackend>, config: AgentConfig) -> Self {
        Self {
            backend,
            tools: HashMap::new(),
            config,
            memory: crate::context::ConversationMemory::new(),
            #[cfg(feature = "explain")]
            recorder: None,
            #[cfg(feature = "explain")]
            explanation_store: None,
        }
    }

    /// 设置对话记忆的最大历史长度
    pub fn with_max_memory(mut self, max_history: usize) -> Self {
        self.memory = crate::context::ConversationMemory::with_max_history(max_history);
        self
    }

    /// 带解释器构造（注册两个 explain Tool + 内部 Recorder）
    ///
    /// **仅在 `explain` feature 启用时可用**。
    #[cfg(feature = "explain")]
    pub fn with_explainer(
        backend: Box<dyn LLMBackend>,
        config: AgentConfig,
        explainer: Arc<dyn axon_explain::traits::Explainer>,
    ) -> Self {
        use crate::explain::{
            ComputeExplanationTool, DecisionRecorder, ExplainerBridge, ExplanationStore,
            QueryExplanationTool,
        };

        let store = Arc::new(ExplanationStore::default());
        let bridge = Arc::new(ExplainerBridge::new(explainer, Arc::clone(&store)));
        let recorder = DecisionRecorder::new(Arc::clone(&bridge));

        let mut agent = Self::new(backend, config);
        agent.recorder = Some(recorder);
        agent.explanation_store = Some(Arc::clone(&store));

        // 注册两个 Tool
        agent.tools.insert(
            "compute_explanation".to_string(),
            Arc::new(ComputeExplanationTool::new(bridge, Arc::clone(&store))),
        );
        agent.tools.insert(
            "query_explanation".to_string(),
            Arc::new(QueryExplanationTool::new(Arc::clone(&store))),
        );

        agent
    }

    /// 获取解释存储的 Arc 克隆（`None` 表示未启用 explain feature）
    ///
    /// 返回 `Option<Arc<...>>` 而非 `Option<&Arc<...>>`：调用方拿到的是
    /// **新克隆的 Arc**,可以直接 `Arc::strong_count` 计数、跨线程发送,
    /// 无需再手动 `Arc::clone` 一次。
    #[cfg(feature = "explain")]
    pub fn explanation_store(&self) -> Option<Arc<crate::explain::ExplanationStore>> {
        self.explanation_store.as_ref().map(Arc::clone)
    }

    /// 注册工具
    pub fn add_tool(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.insert(name, tool.into());
    }

    /// ReAct 主循环：推理并执行工具调用
    ///
    /// 流程：
    /// 1. 初始化上下文（system + user）
    /// 2. 循环直到 LLM 返回文本或达到最大轮次
    /// 3. 每次迭代：调用 LLM → 检查工具调用 → 执行工具 → 继续
    pub async fn reason(&mut self, query: &str) -> Result<AgentResponse, AgentError> {
        let mut ctx = ContextManager::new(self.config.max_context_tokens);
        let mut total_usage = TokenUsage::default();
        let mut steps = Vec::new();

        // 系统提示
        ctx.add_message(Message::system(self.build_system_prompt()));
        // 用户问题
        ctx.add_message(Message::user(query));

        // 将用户问题加入记忆
        self.memory.add(Message::user(query.to_string()));

        for iteration in 0..self.config.max_iterations {
            debug!(
                "ReAct 轮次 {}/{}",
                iteration + 1,
                self.config.max_iterations
            );

            // 准备工具定义
            let tool_defs: Vec<ToolDefinition> =
                self.tools.values().map(|t| t.definition()).collect();

            // 调用 LLM
            let response = self
                .backend
                .complete_with_tools(ctx.messages(), &tool_defs)
                .await
                .map_err(|e| AgentError::LLMError(e.to_string()))?;

            total_usage.prompt_tokens += response.token_usage.prompt_tokens;
            total_usage.completion_tokens += response.token_usage.completion_tokens;
            total_usage.total_tokens += response.token_usage.total_tokens;

            let thought = response.content.clone().unwrap_or_default();

            // 检查是否有工具调用
            if let Some(tool_calls) = &response.tool_calls
                && !tool_calls.is_empty()
            {
                let tool_call = &tool_calls[0];
                let tool_name = &tool_call.function_name;

                // 权限检查
                if !self.config.allowed_tools.is_empty()
                    && !self.config.allowed_tools.contains(tool_name)
                {
                    return Err(AgentError::PermissionDenied(format!(
                        "{} 不在允许的工具列表中",
                        tool_name
                    )));
                }

                // 执行工具
                let observation = self.execute_tool(tool_name, &tool_call.arguments).await?;

                let step = ReasoningStep {
                    step: iteration,
                    thought: thought.clone(),
                    action: Some(tool_call.clone()),
                    observation: Some(observation.clone()),
                };
                steps.push(step.clone());

                // 异步触发解释记录（fire-and-forget，不阻塞主循环）
                #[cfg(feature = "explain")]
                if let Some(recorder) = &self.recorder {
                    let action_snapshot = snapshot_from_tool_call(&tool_call.arguments);
                    let record = crate::explain::DecisionRecord {
                        decision_id: uuid::Uuid::new_v4().to_string(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        mode: crate::explain::ExplainMode::WithReasoning,
                        query: query.to_string(),
                        reasoning_trace: vec![step],
                        final_action: action_snapshot,
                    };
                    recorder.record_async(record);
                }

                // 追加到上下文
                ctx.add_message(Message {
                    role: Role::Assistant,
                    content: thought,
                    tool_call_id: None,
                    tool_calls: Some(tool_calls.clone()),
                });
                ctx.add_message(Message::tool_result(&tool_call.id, &observation));

                // 将观察结果加入记忆
                self.memory
                    .add(Message::tool_result(&tool_call.id, &observation));

                continue;
            }

            // 无工具调用 → 最终答案
            steps.push(ReasoningStep {
                step: iteration,
                thought,
                action: None,
                observation: None,
            });

            // 将助手答案加入记忆
            self.memory.add(Message::assistant(
                response.content.clone().unwrap_or_default(),
            ));

            return Ok(AgentResponse {
                answer: response.content.unwrap_or_default(),
                reasoning_trace: steps,
                token_usage: total_usage,
                iterations: iteration + 1,
            });
        }

        Err(AgentError::MaxIterationsExceeded {
            max: self.config.max_iterations,
        })
    }

    /// 执行工具
    async fn execute_tool(&self, name: &str, arguments: &str) -> Result<String, AgentError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| AgentError::ToolError(format!("工具 '{}' 不存在", name)))?;

        tool.execute(arguments)
            .await
            .map_err(|e| AgentError::ToolError(e.to_string()))
    }

    /// 构造系统提示
    fn build_system_prompt(&self) -> String {
        if self.tools.is_empty() {
            return "你是一个有用的助手。请直接回答用户的问题。".to_string();
        }

        let tool_descriptions: Vec<String> = self
            .tools
            .values()
            .map(|t| format!("- {}: {}", t.name(), t.description()))
            .collect();

        format!(
            r#"你是一个交易辅助智能体。请通过 Thought → Action → Observation 的循环来回答问题。

## 思考过程
每次回答前，先进行 Thought 分析：
1. 理解用户意图
2. 确定是否需要工具调用
3. 如果需要，选择合适的工具
4. 执行工具并分析结果

## 可用工具
{tool_desc}

## 规则
1. 每次只调用一个工具
2. 不要编造数据，使用工具获取真实数据
3. 如果工具调用失败，尝试其他方法
4. 最终答案必须基于工具返回的真实数据
5. 不要执行任何未授权操作

## 输出格式
先输出 Thought（分析过程），然后输出 Answer（最终答案）。"#,
            tool_desc = tool_descriptions.join("\n")
        )
    }

    /// 获取对话记忆
    pub fn memory(&self) -> &crate::context::ConversationMemory {
        &self.memory
    }
}

/// 从 `ToolCall.arguments` JSON 解析 `ActionSnapshot`
///
/// 解析失败的字段用 0 / "unknown" 兜底，确保 ReAct 主循环不会因解析错误中断。
#[cfg(feature = "explain")]
fn snapshot_from_tool_call(arguments: &str) -> axon_explain::types::ActionSnapshot {
    use axon_explain::types::ActionSnapshot;

    let v: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    ActionSnapshot {
        position_size: v["position_size"].as_f64().unwrap_or(0.0),
        entry_price: v["entry_price"].as_f64().unwrap_or(0.0),
        stop_loss: v["stop_loss"].as_f64().unwrap_or(0.0),
        take_profit: v["take_profit"].as_f64().unwrap_or(0.0),
        order_type: v["order_type"].as_str().unwrap_or("unknown").to_string(),
    }
}
