//! Policy trait 单元测试
//!
//! 验证自定义策略实现能被集成框架使用。

use axon_ensemble::{Action, ActionProbabilities, EnsembleAction, ModelType, Observation, Policy};

/// 简单规则策略：所有观测都返回 Buy
struct AlwaysBuyPolicy {
    name: String,
    confidence: f64,
}

impl Policy for AlwaysBuyPolicy {
    fn predict(&self, _observation: &Observation) -> Action {
        Action {
            action_type: EnsembleAction::Buy,
            symbol: Some("BTC".to_string()),
            quantity: Some(1.0),
            confidence: self.confidence,
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn model_type(&self) -> ModelType {
        ModelType::RuleBased
    }

    fn action_probs(&self, _observation: &Observation) -> ActionProbabilities {
        ActionProbabilities::new(self.confidence, 0.0, 1.0 - self.confidence)
    }
}

#[test]
fn test_policy_predict_returns_action() {
    let policy = AlwaysBuyPolicy {
        name: "always_buy".to_string(),
        confidence: 0.9,
    };
    let obs = Observation::default();
    let action = policy.predict(&obs);
    assert_eq!(action.action_type, EnsembleAction::Buy);
    assert_eq!(action.symbol.as_deref(), Some("BTC"));
    assert_eq!(action.quantity, Some(1.0));
    assert!((action.confidence - 0.9).abs() < 1e-9);
}

#[test]
fn test_policy_name_and_type() {
    let policy = AlwaysBuyPolicy {
        name: "test_model".to_string(),
        confidence: 0.5,
    };
    assert_eq!(policy.name(), "test_model");
    assert_eq!(policy.model_type(), ModelType::RuleBased);
}

#[test]
fn test_policy_action_probs_optional() {
    // 默认实现应返回均匀分布
    struct DefaultPolicy;
    impl Policy for DefaultPolicy {
        fn predict(&self, _observation: &Observation) -> Action {
            Action {
                action_type: EnsembleAction::Hold,
                symbol: None,
                quantity: None,
                confidence: 0.0,
            }
        }
        fn name(&self) -> &str {
            "default"
        }
        fn model_type(&self) -> ModelType {
            ModelType::PPO
        }
    }

    let obs = Observation::default();
    let probs = DefaultPolicy.action_probs(&obs);
    // 均匀分布：1/3, 1/3, 1/3
    assert!((probs.buy - 1.0 / 3.0).abs() < 1e-9);
    assert!((probs.sell - 1.0 / 3.0).abs() < 1e-9);
    assert!((probs.hold - 1.0 / 3.0).abs() < 1e-9);
}

#[test]
fn test_policy_trait_is_object_safe() {
    // 编译期验证：Policy 可以用作 dyn trait object
    let _policy: Box<dyn Policy> = Box::new(AlwaysBuyPolicy {
        name: "boxed".to_string(),
        confidence: 0.7,
    });
}
