//! EnsembleManager 单元测试
//!
//! 验证统一管理多个模型 + 多样性度量

use axon_ensemble::{
    Action, ActionProbabilities, EnsembleAction, EnsembleManager, HardVoteStrategy, Observation,
    Policy,
};
struct FixedPolicy {
    name: String,
    action_type: EnsembleAction,
}

impl Policy for FixedPolicy {
    fn predict(&self, _observation: &Observation) -> Action {
        Action {
            action_type: self.action_type,
            symbol: Some("BTC".to_string()),
            quantity: Some(1.0),
            confidence: 0.7,
        }
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn model_type(&self) -> axon_ensemble::ModelType {
        axon_ensemble::ModelType::PPO
    }
    fn action_probs(&self, _observation: &Observation) -> ActionProbabilities {
        let (b, s, h) = match self.action_type {
            EnsembleAction::Buy => (0.7, 0.1, 0.2),
            EnsembleAction::Sell => (0.1, 0.7, 0.2),
            EnsembleAction::Hold => (0.2, 0.2, 0.6),
        };
        ActionProbabilities::new(b, s, h)
    }
}

#[test]
fn test_manager_register_model_assigns_uniform_weight() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(Box::new(FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    manager.register_model(Box::new(FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    let weights = manager.get_weights();
    assert_eq!(weights.len(), 2);
    assert!((weights[0].weight - 0.5).abs() < 1e-9);
    assert!((weights[1].weight - 0.5).abs() < 1e-9);
}

#[test]
fn test_manager_predict_uses_voting_strategy() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(Box::new(FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    manager.register_model(Box::new(FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    manager.register_model(Box::new(FixedPolicy {
        name: "m3".to_string(),
        action_type: EnsembleAction::Sell,
    }));
    let action = manager.predict(&Observation::default(), 1000);
    assert_eq!(action.action_type, EnsembleAction::Buy);
}

#[test]
fn test_manager_diversity_zero_for_identical_models() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(Box::new(FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    manager.register_model(Box::new(FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    let obs = vec![Observation::default(), Observation::default()];
    let diversity = manager.compute_diversity(&obs);
    assert!(
        (diversity - 0.0).abs() < 1e-9,
        "相同模型多样性应为 0，实际 {}",
        diversity
    );
}

#[test]
fn test_manager_diversity_one_for_disagreeing_models() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(Box::new(FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    manager.register_model(Box::new(FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Sell,
    }));
    let obs = vec![Observation::default()];
    let diversity = manager.compute_diversity(&obs);
    assert!(
        (diversity - 1.0).abs() < 1e-9,
        "完全分歧多样性应为 1.0，实际 {}",
        diversity
    );
}

#[test]
fn test_manager_diversity_single_model_returns_zero() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(Box::new(FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    let obs = vec![Observation::default()];
    let diversity = manager.compute_diversity(&obs);
    assert!((diversity - 0.0).abs() < 1e-9);
}

#[test]
fn test_manager_diversity_empty_observations_returns_zero() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(Box::new(FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    manager.register_model(Box::new(FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Sell,
    }));
    let diversity = manager.compute_diversity(&[]);
    assert!((diversity - 0.0).abs() < 1e-9);
}

#[test]
fn test_manager_predict_records_history() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(Box::new(FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    let _ = manager.predict(&Observation::default(), 1000);
    let _ = manager.predict(&Observation::default(), 2000);
    let _ = manager.predict(&Observation::default(), 3000);
    assert_eq!(manager.history_len(), 3);
}

#[test]
fn test_manager_update_weights_propagates_to_strategy() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(Box::new(FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    manager.register_model(Box::new(FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Sell,
    }));
    // 调用不应崩溃
    manager.update_weights(&[0.8, 0.2]);
}

#[test]
fn test_manager_set_weights() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(Box::new(FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
    }));
    manager.register_model(Box::new(FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Sell,
    }));
    manager.set_weights(vec![0.7, 0.3]);
    let weights = manager.get_weights();
    assert!((weights[0].weight - 0.7).abs() < 1e-9);
    assert!((weights[1].weight - 0.3).abs() < 1e-9);
}
