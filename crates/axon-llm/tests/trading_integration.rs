//! 交易工具与 ReActAgent 闭环集成测试
//!
//! 使用本地 ScriptedMock(LLMBackend 简单实现) + MockTradingBackend 验证:
//! - Tool 被 ReActAgent 正确调用
//! - Observation 包含期望字段
//! - TwoPhase 模式可在两个 ReAct 轮次内完成
//!
//! 注:不使用 `axon_llm::backends::mock::MockBackend`,因为它在 `backends` feature
//! 下(默认关闭,本集成测试在 default features 下运行)。

use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;
use axon_llm::AgentConfig;
use axon_llm::backend::{LLMBackend, LLMError, ToolDefinition};
use axon_llm::react_agent::ReActAgent;
use axon_llm::tools::Tool;
use axon_llm::trading::{
    DailyCounter, MockTradingBackend, OrderAck, PlaceOrderTool, QueryPortfolioTool, RiskLimits,
    SafetyMode,
};
use axon_llm::types::{LLMResponse, Message, TokenUsage, ToolCall};

/// 按预定义响应序列消费的 mock LLM 后端
struct ScriptedMock {
    responses: StdMutex<Vec<LLMResponse>>,
}

impl ScriptedMock {
    fn new(responses: Vec<LLMResponse>) -> Self {
        Self {
            responses: StdMutex::new(responses),
        }
    }
}

#[async_trait]
impl LLMBackend for ScriptedMock {
    async fn complete(&self, _messages: &[Message]) -> Result<LLMResponse, LLMError> {
        let mut g = self.responses.lock().expect("poisoned");
        g.pop().ok_or(LLMError::MockExhausted)
    }

    async fn complete_with_tools(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse, LLMError> {
        self.complete(_messages).await
    }

    fn context_window_size(&self) -> usize {
        8192
    }
}

fn mk_tool_call(id: &str, name: &str, args: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        function_name: name.into(),
        arguments: args.into(),
    }
}

fn mk_response_with_tool_call(tc: &ToolCall) -> LLMResponse {
    LLMResponse {
        content: Some(format!("call {}", tc.function_name)),
        tool_calls: Some(vec![tc.clone()]),
        token_usage: TokenUsage::new(0, 0),
        finish_reason: axon_llm::types::FinishReason::ToolCalls,
    }
}

fn mk_response_text(text: &str) -> LLMResponse {
    LLMResponse::text(text, TokenUsage::new(0, 0))
}

fn mk_config() -> AgentConfig {
    AgentConfig {
        allowed_tools: vec!["place_order".into(), "query_portfolio".into()],
        max_iterations: 5,
        ..Default::default()
    }
}

#[tokio::test]
async fn agent_place_order_dry_run_observation() {
    let m = Arc::new(MockTradingBackend::new());
    let tool = PlaceOrderTool::new(m.clone(), SafetyMode::DryRun, RiskLimits::permissive(), Arc::new(DailyCounter::default()));

    let tc = mk_tool_call(
        "call-1",
        "place_order",
        r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.1,"price":50000.0}"#,
    );
    let backend = Box::new(ScriptedMock::new(vec![
        mk_response_text("已记录 dry-run 订单"),
        mk_response_with_tool_call(&tc),
    ]));

    let mut agent = ReActAgent::new(backend, mk_config());
    agent.add_tool(Box::new(tool));

    let resp = agent.reason("买 0.1 BTC").await.unwrap();
    assert!(resp.answer.contains("dry-run"));
    assert_eq!(m.order_count(), 0); // DryRun 不真发
    assert!(resp.iterations >= 2); // tool_call 轮 + 最终答案轮
}

#[tokio::test]
async fn agent_query_portfolio_in_observation() {
    let m = Arc::new(MockTradingBackend::new());
    let tool = QueryPortfolioTool::new(m);

    let tc = mk_tool_call("call-1", "query_portfolio", r#"{"symbol":"BTC-USDT"}"#);
    // pop 从队尾取,所以 push 顺序是反的(先 push 最终答案,后 push tc)
    let backend = Box::new(ScriptedMock::new(vec![
        mk_response_text("你的 BTC 持仓 0.1"),
        mk_response_with_tool_call(&tc),
    ]));

    let mut agent = ReActAgent::new(backend, mk_config());
    agent.add_tool(Box::new(tool));

    let resp = agent.reason("我有多少 BTC").await.unwrap();
    assert!(resp.answer.contains("BTC 持仓 0.1"));
    let trace = &resp.reasoning_trace;
    assert!(!trace.is_empty());
    // observation 应在含 tool_call 的那一轮(trace 的倒数第二步)
    let tool_step = trace
        .iter()
        .rev()
        .find(|s| s.action.is_some())
        .expect("应有 tool_call 步骤");
    let last_obs = tool_step.observation.as_deref().unwrap_or("");
    assert!(
        last_obs.contains("USDT"),
        "observation 应含 USDT: {}",
        last_obs
    );
    let v: serde_json::Value = serde_json::from_str(last_obs).unwrap();
    let positions = v["positions"].as_array().unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0]["symbol"], "BTC-USDT");
}

#[tokio::test]
async fn agent_two_phase_full_cycle() {
    let m = Arc::new(MockTradingBackend::new());
    let tool = PlaceOrderTool::new(m.clone(), SafetyMode::TwoPhase, RiskLimits::permissive(), Arc::new(DailyCounter::default()));

    // 预生成 confirm_token(Mock 不知道 LLM 在 Observation 里看到什么,需要预先生成)
    let pre = tool
        .execute(r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.1,"price":50000.0}"#)
        .await
        .unwrap();
    let pre_ack: OrderAck = serde_json::from_str(&pre).unwrap();
    let token = pre_ack.confirm_token.expect("token");

    let tc1 = mk_tool_call(
        "call-1",
        "place_order",
        r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.1,"price":50000.0}"#,
    );
    let tc2_args = serde_json::json!({
        "symbol": "BTC-USDT",
        "side": "Buy",
        "quantity": 0.1,
        "price": 50_000.0,
        "extras": {"confirm_token": token}
    })
    .to_string();
    let tc2 = mk_tool_call("call-2", "place_order", &tc2_args);

    // ReAct 流程:thought -> action(LLM 返回 tc) -> observation(实际 tool 结果) -> 下一轮
    // pop 从队尾取,所以 push 顺序是反的(先 push 最终答案,后 push tc1)
    let backend = Box::new(ScriptedMock::new(vec![
        mk_response_text("已确认下单"),
        mk_response_with_tool_call(&tc2),
        mk_response_with_tool_call(&tc1),
    ]));

    let mut agent = ReActAgent::new(backend, mk_config());
    agent.add_tool(Box::new(tool));

    let resp = agent.reason("买 0.1 BTC 走两步确认").await.unwrap();
    assert!(resp.answer.contains("确认"));
    assert_eq!(m.order_count(), 1); // 真实订单 1 条
    assert_eq!(resp.iterations, 3); // tc1 + tc2 + final
}
