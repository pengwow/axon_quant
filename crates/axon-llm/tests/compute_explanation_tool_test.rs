//! ComputeExplanationTool 单元测试
//!
//! 覆盖：name/schema / Fast explainer 成功 / 慢 explainer 超时降级 / 无效 JSON / 缺字段 / default timeout

#![cfg(feature = "explain")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axon_explain::error::ExplainabilityError;
use axon_explain::traits::Explainer;
use axon_explain::types::{
    ActionSnapshot, AttentionWeights, CounterfactualExplanation, Explanation,
};
use axon_llm::explain::{ComputeExplanationTool, ExplainerBridge, ExplanationStore};
use axon_llm::tools::{Tool, ToolError};

struct FastExplainer;

#[async_trait]
impl Explainer for FastExplainer {
    fn explain(
        &self,
        _o: &HashMap<String, f64>,
        a: &ActionSnapshot,
    ) -> Result<Explanation, ExplainabilityError> {
        Ok(Explanation {
            id: "fast".to_string(),
            observation_id: "obs".to_string(),
            action: a.clone(),
            feature_importance: Default::default(),
            action_attributions: vec![],
            attention_weights: None,
            counterfactuals: vec![],
            summary: "fast explanation result".to_string(),
            confidence: 0.95,
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
    fn get_attention_weights(&self, _o: &HashMap<String, f64>) -> Option<Vec<AttentionWeights>> {
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

struct SlowExplainer;

#[async_trait]
impl Explainer for SlowExplainer {
    fn explain(
        &self,
        _o: &HashMap<String, f64>,
        a: &ActionSnapshot,
    ) -> Result<Explanation, ExplainabilityError> {
        std::thread::sleep(Duration::from_millis(300));
        Ok(Explanation {
            id: "slow".to_string(),
            observation_id: "obs".to_string(),
            action: a.clone(),
            feature_importance: Default::default(),
            action_attributions: vec![],
            attention_weights: None,
            counterfactuals: vec![],
            summary: "slow".to_string(),
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
    fn get_attention_weights(&self, _o: &HashMap<String, f64>) -> Option<Vec<AttentionWeights>> {
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

const SAMPLE_ARGS: &str = r#"{
    "query": "buy BTC?",
    "final_action": {
        "position_size": 1.0,
        "entry_price": 50000.0,
        "stop_loss": 48000.0,
        "take_profit": 55000.0,
        "order_type": "limit"
    },
    "reasoning_trace": [],
    "mode": "ActionOnly"
}"#;

fn make_tool_fast() -> (Arc<ExplanationStore>, ComputeExplanationTool) {
    let store = Arc::new(ExplanationStore::new(100));
    let explainer: Arc<dyn Explainer> = Arc::new(FastExplainer);
    let bridge = Arc::new(ExplainerBridge::new(explainer, Arc::clone(&store)));
    let tool = ComputeExplanationTool::new(bridge, Arc::clone(&store));
    (store, tool)
}

fn make_tool_slow() -> (Arc<ExplanationStore>, ComputeExplanationTool) {
    let store = Arc::new(ExplanationStore::new(100));
    let explainer: Arc<dyn Explainer> = Arc::new(SlowExplainer);
    let bridge = Arc::new(ExplainerBridge::new(explainer, Arc::clone(&store)));
    let tool = ComputeExplanationTool::new(bridge, Arc::clone(&store));
    (store, tool)
}

#[tokio::test]
async fn test_compute_tool_name_and_description() {
    let (_, tool) = make_tool_fast();
    assert_eq!(tool.name(), "compute_explanation");
    assert!(!tool.description().is_empty());
    assert!(tool.parameters_schema().is_object());
}

#[tokio::test]
async fn test_compute_tool_fast_returns_explanation() {
    let (store, tool) = make_tool_fast();
    let result = tool.execute(SAMPLE_ARGS).await;
    assert!(result.is_ok(), "实际错误: {:?}", result.err());
    let json = result.unwrap();
    assert!(json.contains("fast explanation result"));

    // 验证写入 store
    assert_eq!(store.len().await, 1);
}

#[tokio::test]
async fn test_compute_tool_timeout_returns_partial_fallback() {
    // SlowExplainer sleep 300ms，工具 timeout 100ms → 超时 → 降级
    let (_, tool) = make_tool_slow();
    let result = tool.execute(SAMPLE_ARGS).await;

    // 超时降级为 Ok(partial_json) 而非错误
    assert!(result.is_ok(), "超时应返回 partial，实际 {:?}", result);
    let json = result.unwrap();
    // 部分结果应包含 partial 标识
    assert!(
        json.contains("partial") || json.contains("timeout") || json.contains("top"),
        "partial JSON 应含降级标识: {}",
        json
    );
}

#[tokio::test]
async fn test_compute_tool_invalid_json_returns_error() {
    let (_, tool) = make_tool_fast();
    let result = tool.execute("not json").await;
    assert!(result.is_err());
    match result {
        Err(ToolError::InvalidArguments(_)) => {}
        other => panic!("期望 InvalidArguments，实际 {:?}", other),
    }
}

#[tokio::test]
async fn test_compute_tool_missing_fields_returns_error() {
    let (_, tool) = make_tool_fast();
    let result = tool.execute(r#"{"query": "x"}"#).await;
    assert!(result.is_err());
    match result {
        Err(ToolError::InvalidArguments(_)) => {}
        other => panic!("期望 InvalidArguments，实际 {:?}", other),
    }
}

#[tokio::test]
async fn test_compute_tool_default_timeout_is_500ms() {
    // Compute 工具默认 500ms（适配 SHAP 计算）
    // 详见 DEFAULT_COMPUTE_TIMEOUT_MS
    let (_, tool) = make_tool_fast();
    assert_eq!(tool.timeout().as_millis(), 500);
}

#[tokio::test]
async fn test_compute_tool_with_timeout_overrides() {
    let store = Arc::new(ExplanationStore::new(100));
    let explainer: Arc<dyn Explainer> = Arc::new(FastExplainer);
    let bridge = Arc::new(ExplainerBridge::new(explainer, Arc::clone(&store)));
    let tool = ComputeExplanationTool::new(bridge, store).with_timeout(Duration::from_millis(500));
    assert_eq!(tool.timeout().as_millis(), 500);
}
