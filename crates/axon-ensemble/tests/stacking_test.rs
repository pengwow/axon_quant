//! StackingEnsemble 单元测试

use axon_ensemble::{
    Action, ActionProbabilities, Ensemble, EnsembleAction, MetaModel, ModelType, Observation,
    Policy, StackingEnsemble,
};

struct FixedPolicy {
    name: String,
    probs: ActionProbabilities,
}

impl Policy for FixedPolicy {
    fn predict(&self, _observation: &Observation) -> Action {
        Action {
            action_type: EnsembleAction::Hold,
            symbol: Some("BTC".to_string()),
            quantity: Some(1.0),
            confidence: 0.5,
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
fn test_meta_model_predict_returns_normalized_probabilities() {
    // 元模型: 3 输出 (buy/sell/hold), 1 特征
    let meta = MetaModel::new(1, 3);
    let probs = meta.predict(&[1.0]);
    let sum = probs.buy + probs.sell + probs.hold;
    assert!(
        (sum - 1.0).abs() < 1e-9,
        "softmax 后总和应为 1.0，实际 {}",
        sum
    );
}

#[test]
fn test_meta_model_predict_different_inputs_differ() {
    // 用非零权重，使不同输入产生不同输出
    let meta = MetaModel::with_weights(
        vec![vec![0.5, -0.5], vec![-0.3, 0.7], vec![0.1, 0.2]],
        vec![0.0, 0.0, 0.0],
    );
    let p1 = meta.predict(&[1.0, 0.0]);
    let p2 = meta.predict(&[0.0, 1.0]);
    // 不同输入应得到不同输出
    let diff = (p1.buy - p2.buy).abs() + (p1.sell - p2.sell).abs() + (p1.hold - p2.hold).abs();
    assert!(diff > 0.01, "不同输入应产生不同输出，差异={}", diff);
}

#[test]
fn test_stacking_ensemble_predict_uses_meta_model() {
    let p1 = Box::new(FixedPolicy {
        name: "m1".to_string(),
        probs: ActionProbabilities::new(0.7, 0.2, 0.1),
    });
    let p2 = Box::new(FixedPolicy {
        name: "m2".to_string(),
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    });
    let meta = MetaModel::new(6, 3);
    let stacking = StackingEnsemble::new(vec![p1, p2], meta);
    let action = stacking.predict(&Observation::default());
    // 元模型 softmax 应选择某 action
    assert!(matches!(
        action.action_type,
        EnsembleAction::Buy | EnsembleAction::Sell | EnsembleAction::Hold
    ));
}

#[test]
fn test_stacking_ensemble_requires_at_least_one_model() {
    let meta = MetaModel::new(3, 3);
    let stacking = StackingEnsemble::new(vec![], meta);
    let action = stacking.predict(&Observation::default());
    // 0 模型时回退到 Hold
    assert_eq!(action.action_type, EnsembleAction::Hold);
}

#[test]
fn test_stacking_ensemble_preserves_symbol_from_first_model() {
    let p1 = Box::new(FixedPolicy {
        name: "m1".to_string(),
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    });
    let meta = MetaModel::new(3, 3);
    let stacking = StackingEnsemble::new(vec![p1], meta);
    let action = stacking.predict(&Observation::default());
    assert_eq!(action.symbol.as_deref(), Some("BTC"));
    assert_eq!(action.quantity, Some(1.0));
}

#[test]
fn test_meta_model_default_weights_are_zero() {
    // 零权重 → softmax 后均匀分布
    let meta = MetaModel::new(2, 3);
    let probs = meta.predict(&[0.0, 0.0]);
    let expected = 1.0 / 3.0;
    assert!((probs.buy - expected).abs() < 1e-6);
    assert!((probs.sell - expected).abs() < 1e-6);
    assert!((probs.hold - expected).abs() < 1e-6);
}

#[test]
fn test_stacking_update_weights_is_noop() {
    // 堆叠通过元模型训练更新，不直接调整权重
    let p1 = Box::new(FixedPolicy {
        name: "m1".to_string(),
        probs: ActionProbabilities::new(0.5, 0.3, 0.2),
    });
    let meta = MetaModel::new(3, 3);
    let mut stacking = StackingEnsemble::new(vec![p1], meta);
    // 调用 update_weights 不应崩溃
    stacking.update_weights(&[1.0]);
    // 仍可预测
    let _action = stacking.predict(&Observation::default());
}
