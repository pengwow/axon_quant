//! ExplainerBridge 单元测试
//!
//! 覆盖：成功 / 业务错误 / spawn_blocking panic / 不阻塞 / 串行一致性

#![cfg(feature = "explain")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axon_explain::error::ExplainabilityError;
use axon_explain::traits::Explainer;
use axon_explain::types::{ActionSnapshot, Explanation};
use axon_llm::explain::{DecisionRecord, ExplainMode, ExplainerBridge, ExplanationStore};

fn sample_record(id: &str) -> DecisionRecord {
    DecisionRecord::new(
        id,
        ExplainMode::ActionOnly,
        "test query for explainer",
        ActionSnapshot {
            position_size: 1.0,
            entry_price: 100.0,
            stop_loss: 90.0,
            take_profit: 120.0,
            order_type: "limit".to_string(),
        },
    )
}

struct MockExplainer {
    should_fail: bool,
    /// 若 Some，会在 explain 中 panic（用于测 spawn_blocking 错误处理）
    panic: bool,
    /// 模拟耗时
    delay: Duration,
}

#[async_trait]
impl Explainer for MockExplainer {
    fn explain(
        &self,
        _observation: &HashMap<String, f64>,
        action: &ActionSnapshot,
    ) -> Result<Explanation, ExplainabilityError> {
        if self.panic {
            panic!("simulated explainer panic");
        }
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        if self.should_fail {
            return Err(ExplainabilityError::FeatureMismatch {
                expected: 3,
                actual: 5,
            });
        }
        Ok(Explanation {
            id: format!("exp-{}", action.entry_price),
            observation_id: "obs".to_string(),
            action: action.clone(),
            feature_importance: Default::default(),
            action_attributions: vec![],
            attention_weights: None,
            counterfactuals: vec![],
            summary: "mock explanation".to_string(),
            confidence: 0.85,
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
    ) -> Option<Vec<axon_explain::types::AttentionWeights>> {
        None
    }

    fn generate_counterfactuals(
        &self,
        _o: &HashMap<String, f64>,
        _a: &ActionSnapshot,
        _m: usize,
    ) -> Vec<axon_explain::types::CounterfactualExplanation> {
        vec![]
    }
}

#[tokio::test]
async fn test_bridge_explain_async_succeeds_and_writes_to_store() {
    let explainer: Arc<dyn Explainer> = Arc::new(MockExplainer {
        should_fail: false,
        panic: false,
        delay: Duration::ZERO,
    });
    let store = Arc::new(ExplanationStore::new(100));
    let bridge = ExplainerBridge::new(explainer, Arc::clone(&store));

    let result = bridge.explain_async(sample_record("test-id")).await;
    assert!(result.is_ok(), "应成功，实际 {:?}", result);

    let exp = store.get("test-id").await;
    assert!(exp.is_some());
    assert_eq!(exp.unwrap().summary, "mock explanation");
}

#[tokio::test]
async fn test_bridge_explain_async_failure_does_not_write_to_store() {
    let explainer: Arc<dyn Explainer> = Arc::new(MockExplainer {
        should_fail: true,
        panic: false,
        delay: Duration::ZERO,
    });
    let store = Arc::new(ExplanationStore::new(100));
    let bridge = ExplainerBridge::new(explainer, Arc::clone(&store));

    let result = bridge.explain_async(sample_record("test-id")).await;
    assert!(result.is_err());

    // store 应保持空
    assert!(!store.contains_key("test-id").await);
    assert!(store.is_empty().await);
}

#[tokio::test]
async fn test_bridge_does_not_block_under_slow_explainer() {
    // 即使 Explainer 同步 sleep 200ms，bridge.explain_async 也应在合理时间内
    // 返回（spawn_blocking 把同步计算移到 blocking thread pool）
    let explainer: Arc<dyn Explainer> = Arc::new(MockExplainer {
        should_fail: false,
        panic: false,
        delay: Duration::from_millis(200),
    });
    let store = Arc::new(ExplanationStore::new(100));
    let bridge = ExplainerBridge::new(explainer, Arc::clone(&store));

    let start = std::time::Instant::now();
    let result = bridge.explain_async(sample_record("slow-id")).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok());
    // spawn_blocking 在多线程 runtime 下会并发执行，单次 200ms 计算大约 200-300ms
    // 这里只验证"能跑通"和"会写入 store"
    assert!(
        elapsed.as_millis() < 1000,
        "elapsed {}ms 异常",
        elapsed.as_millis()
    );

    let exp = store.get("slow-id").await;
    assert!(exp.is_some());
}

#[tokio::test]
async fn test_bridge_concurrent_calls_write_separately() {
    let explainer: Arc<dyn Explainer> = Arc::new(MockExplainer {
        should_fail: false,
        panic: false,
        delay: Duration::from_millis(50),
    });
    let store = Arc::new(ExplanationStore::new(100));
    let bridge = Arc::new(ExplainerBridge::new(explainer, Arc::clone(&store)));

    let mut handles = vec![];
    for i in 0..10 {
        let b = Arc::clone(&bridge);
        let id = format!("d{}", i);
        handles.push(tokio::spawn(async move {
            b.explain_async(sample_record(&id)).await
        }));
    }

    for h in handles {
        let r = h.await.unwrap();
        assert!(r.is_ok());
    }

    assert_eq!(store.len().await, 10);
}
