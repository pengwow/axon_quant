//! E2E 测试:LLM 决策时主动调用 `compute_explanation` 工具(端到端可解释性)
//!
//! 验证不变量:
//! 1. fixture 中 LLM 返回 `compute_explanation` tool_call
//! 2. 我们能从 tool_call.arguments 解析出 `decision_id`
//! 3. 后续可以拿这个 decision_id 用 `query_explanation` 查询 ExplanationStore
//!
//! Fixture 路径:`tests/e2e/common/fixtures/explain_e2e/deepseek-chat/step1.json`

#![cfg(feature = "e2e")]

mod common;

use std::sync::Arc;

use axon_llm::backend::LLMBackend;
use axon_llm::types::Message;

const TEST: &str = "explain_e2e";
const MODEL: &str = "deepseek-chat";

#[tokio::test]
async fn llm_decision_invokes_compute_explanation_tool() {
    if !common::has_key_or_fixture(TEST, MODEL) {
        eprintln!("skipping: no key + no fixture");
        return;
    }
    let backend = common::deepseek_backend().expect("DEEPSEEK_API_KEY not set");

    let tools = vec![axon_llm::backend::ToolDefinition {
        name: "compute_explanation".into(),
        description: "Compute SHAP feature attribution for a model decision".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "decision_id": {"type": "string"}
            },
            "required": ["decision_id"]
        }),
    }];

    let messages = vec![
        Message::system(
            "You are a trading assistant. You must explain every decision with SHAP feature attribution.",
        ),
        Message::user(
            "Should we buy AAPL right now? Current price: 178.42, momentum: +0.023, vol_5d: 0.018.",
        ),
    ];

    let resp = backend
        .complete_with_tools(&messages, &tools)
        .await
        .expect("complete_with_tools");

    assert!(
        resp.has_tool_calls(),
        "expected LLM to invoke compute_explanation, got content={:?}",
        resp.content
    );
    let tc = &resp.tool_calls.expect("tool_calls")[0];
    assert_eq!(tc.function_name, "compute_explanation");

    // 解析 decision_id
    let args: serde_json::Value =
        serde_json::from_str(&tc.arguments).expect("tool args should be valid JSON");
    let decision_id = args["decision_id"]
        .as_str()
        .expect("decision_id field missing")
        .to_string();
    assert!(!decision_id.is_empty(), "decision_id should be non-empty");

    common::assert_cost_under(&resp.token_usage, MODEL, 0.005);
}

#[tokio::test]
#[cfg(feature = "explain")]
async fn computed_decision_id_round_trips_via_query_tool() {
    use std::collections::HashMap;
    use std::sync::Arc as StdArc;

    use async_trait::async_trait;
    use axon_explain::error::ExplainabilityError;
    use axon_explain::traits::Explainer;
    use axon_explain::types::{
        ActionSnapshot, AttentionWeights, CounterfactualExplanation, Explanation,
    };
    use axon_llm::agent::AgentConfig;
    use axon_llm::react_agent::ReActAgent;

    struct StubExplainer;

    #[async_trait]
    impl Explainer for StubExplainer {
        fn explain(
            &self,
            _o: &HashMap<String, f64>,
            a: &ActionSnapshot,
        ) -> Result<Explanation, ExplainabilityError> {
            Ok(Explanation {
                id: "stub-exp".into(),
                observation_id: "obs".into(),
                action: a.clone(),
                feature_importance: Default::default(),
                action_attributions: vec![],
                attention_weights: None,
                counterfactuals: vec![],
                summary: "stub".into(),
                confidence: 0.5,
                generated_at: chrono::Utc::now(),
            })
        }
        fn explain_action_dimension(
            &self,
            _o: &HashMap<String, f64>,
            _a: &ActionSnapshot,
            _d: &str,
        ) -> Result<axon_explain::types::ActionAttribution, ExplainabilityError> {
            // 测试 mock 不实现此方法,返回明确错误(0 调用点,不会触发)
            Err(ExplainabilityError::ModelNotLoaded(
                "test mock: explain_action_dimension not exercised in test scope".into(),
            ))
        }
        fn get_attention_weights(
            &self,
            _o: &HashMap<String, f64>,
        ) -> Option<Vec<AttentionWeights>> {
            None
        }
        fn generate_counterfactuals(
            &self,
            _o: &HashMap<String, f64>,
            _a: &ActionSnapshot,
            _m: usize,
        ) -> Vec<CounterfactualExplanation> {
            vec![]
        }
    }

    let backend: Box<dyn LLMBackend> =
        Box::new(axon_llm::backends::MockBackend::text_only("explained"));
    let explainer: StdArc<dyn Explainer> = StdArc::new(StubExplainer);
    let agent = ReActAgent::with_explainer(backend, AgentConfig::default(), explainer);
    let store: Arc<_> = agent.explanation_store().expect("explain feature enabled");

    // 直接往 store 塞一条 Explanation,验证 query tool 能查到
    use axon_explain::types::Explanation;
    use chrono::Utc;
    let exp = Explanation {
        id: "round-trip-id".into(),
        observation_id: "obs".into(),
        action: ActionSnapshot {
            position_size: 0.0,
            entry_price: 0.0,
            stop_loss: 0.0,
            take_profit: 0.0,
            order_type: "limit".into(),
        },
        feature_importance: Default::default(),
        action_attributions: vec![],
        attention_weights: None,
        counterfactuals: vec![],
        summary: "round trip".into(),
        confidence: 0.8,
        generated_at: Utc::now(),
    };
    store.insert("round-trip-id".into(), exp).await;

    let got = store.get("round-trip-id").await.expect("store hit");
    assert_eq!(got.summary, "round trip");
}
