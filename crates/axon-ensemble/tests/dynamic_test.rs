//! DynamicWeightedEnsemble 单元测试

use axon_ensemble::{
    Action, ActionProbabilities, DynamicWeightedEnsemble, Ensemble, EnsembleAction,
    ModelPerformance, ModelType, Observation, Policy,
};

struct FixedPolicy {
    name: String,
    action_type: EnsembleAction,
    probs: ActionProbabilities,
}

impl Policy for FixedPolicy {
    fn predict(&self, _observation: &Observation) -> Action {
        Action {
            action_type: self.action_type,
            symbol: Some("BTC".to_string()),
            quantity: Some(1.0),
            confidence: 0.8,
        }
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn model_type(&self) -> ModelType {
        ModelType::PPO
    }
    fn action_probs(&self, _observation: &Observation) -> ActionProbabilities {
        self.probs.clone()
    }
}

#[test]
fn test_dynamic_initial_weights_are_uniform() {
    let p1 = FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    };
    let p2 = FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    };
    let ens = DynamicWeightedEnsemble::new(vec![Box::new(p1), Box::new(p2)], 0.95, 0.1);
    let weights = ens.get_weights();
    assert_eq!(weights.len(), 2);
    // 均匀权重
    assert!((weights[0].weight - 0.5).abs() < 1e-9);
    assert!((weights[1].weight - 0.5).abs() < 1e-9);
    // 权重和 = 1
    let sum: f64 = weights.iter().map(|w| w.weight).sum();
    assert!((sum - 1.0).abs() < 1e-9);
}

#[test]
fn test_dynamic_predict_with_uniform_weights() {
    let p1 = FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.6, 0.2, 0.2),
    };
    let p2 = FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.4, 0.4, 0.2),
    };
    let ens = DynamicWeightedEnsemble::new(vec![Box::new(p1), Box::new(p2)], 0.95, 0.1);
    let action = ens.predict(&Observation::default());
    assert_eq!(action.action_type, EnsembleAction::Buy);
}

#[test]
fn test_dynamic_weights_shift_toward_better_performers() {
    // 模型 1 表现好 (高 sharpe)，模型 2 表现差 (负 sharpe)
    let p1 = FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    };
    let p2 = FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    };
    let mut ens = DynamicWeightedEnsemble::new(vec![Box::new(p1), Box::new(p2)], 0.95, 0.1);

    // 添加表现记录
    ens.update_performance(ModelPerformance {
        model_name: "m1".to_string(),
        accuracy: 0.9,
        sharpe_ratio: 2.0,
        max_drawdown: -0.1,
        total_return: 0.5,
        sample_count: 100,
        last_evaluated: 1000,
    });
    ens.update_performance(ModelPerformance {
        model_name: "m2".to_string(),
        accuracy: 0.3,
        sharpe_ratio: -0.5,
        max_drawdown: -0.8,
        total_return: -0.3,
        sample_count: 100,
        last_evaluated: 1000,
    });

    let weights = ens.get_weights();
    // 模型 1 权重应 > 模型 2
    let w1 = weights
        .iter()
        .find(|w| w.model_name == "m1")
        .unwrap()
        .weight;
    let w2 = weights
        .iter()
        .find(|w| w.model_name == "m2")
        .unwrap()
        .weight;
    assert!(w1 > w2, "m1 权重 {} 应 > m2 权重 {}", w1, w2);
}

#[test]
fn test_dynamic_update_weights_via_performances_array() {
    let p1 = FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    };
    let p2 = FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    };
    let mut ens = DynamicWeightedEnsemble::new(vec![Box::new(p1), Box::new(p2)], 0.95, 0.0);
    // 2 个标量表现值，与模型顺序对应
    ens.update_weights(&[0.9, 0.1]);
    let weights = ens.get_weights();
    let w1 = weights
        .iter()
        .find(|w| w.model_name == "m1")
        .unwrap()
        .weight;
    let w2 = weights
        .iter()
        .find(|w| w.model_name == "m2")
        .unwrap()
        .weight;
    assert!(w1 > w2);
}

#[test]
fn test_dynamic_history_capped_at_1000() {
    let p1 = FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    };
    let mut ens = DynamicWeightedEnsemble::new(vec![Box::new(p1)], 0.95, 0.1);
    // 推入 1500 条表现记录
    for i in 0..1500 {
        ens.update_performance(ModelPerformance {
            model_name: "m1".to_string(),
            accuracy: 0.5,
            sharpe_ratio: 1.0,
            max_drawdown: -0.1,
            total_return: 0.0,
            sample_count: 1,
            last_evaluated: i as u64,
        });
    }
    assert!(ens.performance_history_len() <= 1000, "历史应被截断到 1000");
}

#[test]
fn test_dynamic_all_zero_sharpe_falls_back_to_uniform() {
    let p1 = FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    };
    let p2 = FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    };
    let mut ens = DynamicWeightedEnsemble::new(vec![Box::new(p1), Box::new(p2)], 0.95, 0.0);
    // 全 0 表现
    ens.update_performance(ModelPerformance {
        model_name: "m1".to_string(),
        accuracy: 0.0,
        sharpe_ratio: 0.0,
        max_drawdown: 0.0,
        total_return: 0.0,
        sample_count: 1,
        last_evaluated: 1,
    });
    ens.update_performance(ModelPerformance {
        model_name: "m2".to_string(),
        accuracy: 0.0,
        sharpe_ratio: 0.0,
        max_drawdown: 0.0,
        total_return: 0.0,
        sample_count: 1,
        last_evaluated: 1,
    });
    let weights = ens.get_weights();
    // 应回退到均匀
    let sum: f64 = weights.iter().map(|w| w.weight).sum();
    assert!((sum - 1.0).abs() < 1e-9);
}

#[test]
fn test_dynamic_predict_uses_weighted_action_probs() {
    // 模型 1: 全 Buy
    // 模型 2: 全 Sell
    // 初始均匀 → 接近 0.5/0.5 → 平票
    let p1 = FixedPolicy {
        name: "m1".to_string(),
        action_type: EnsembleAction::Buy,
        probs: ActionProbabilities::new(1.0, 0.0, 0.0),
    };
    let p2 = FixedPolicy {
        name: "m2".to_string(),
        action_type: EnsembleAction::Sell,
        probs: ActionProbabilities::new(0.0, 1.0, 0.0),
    };
    // m1 权重 0.8, m2 权重 0.2 → Buy 应胜出
    let mut ens = DynamicWeightedEnsemble::new(vec![Box::new(p1), Box::new(p2)], 0.95, 0.1);
    ens.update_weights(&[0.8, 0.2]);
    let action = ens.predict(&Observation::default());
    // 加权 buy = 0.8*1 + 0.2*0 = 0.8
    // 加权 sell = 0.8*0 + 0.2*1 = 0.2
    assert_eq!(action.action_type, EnsembleAction::Buy);
    assert!((action.confidence - 0.8).abs() < 1e-9);
}
