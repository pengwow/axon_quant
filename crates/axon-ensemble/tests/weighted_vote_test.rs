//! WeightedVoteStrategy 单元测试

use axon_ensemble::{
    Action, ActionProbabilities, EnsembleAction, ModelPrediction, ModelType, VotingStrategy,
    WeightedVoteStrategy,
};

fn make_pred(buy: f64, sell: f64, hold: f64, name: &str) -> ModelPrediction {
    ModelPrediction {
        model_name: name.to_string(),
        model_type: ModelType::PPO,
        action: Action {
            action_type: EnsembleAction::Hold,
            symbol: Some("BTC".to_string()),
            quantity: Some(1.0),
            confidence: 0.5,
        },
        confidence: 0.5,
        action_probs: ActionProbabilities::new(buy, sell, hold),
    }
}

#[test]
fn test_weighted_vote_basic() {
    // 模型 1 (权重 0.7): (0.6, 0.2, 0.2)
    // 模型 2 (权重 0.3): (0.2, 0.6, 0.2)
    // 加权 buy = 0.7*0.6 + 0.3*0.2 = 0.48
    // 加权 sell = 0.7*0.2 + 0.3*0.6 = 0.32
    // 加权 hold = 0.7*0.2 + 0.3*0.2 = 0.20
    let preds = vec![
        make_pred(0.6, 0.2, 0.2, "m1"),
        make_pred(0.2, 0.6, 0.2, "m2"),
    ];
    let strategy = WeightedVoteStrategy::new(vec![0.7, 0.3]).unwrap();
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Buy);
    assert!(
        (result.confidence - 0.48).abs() < 1e-9,
        "期望 0.48, 实际 {}",
        result.confidence
    );
}

#[test]
fn test_weighted_vote_weights_must_sum_to_one() {
    // 0.5 + 0.3 = 0.8 ≠ 1 → 错误
    let result = WeightedVoteStrategy::new(vec![0.5, 0.3]);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("权重和不为一"));
}

#[test]
fn test_weighted_vote_uniform_constructor() {
    let strategy = WeightedVoteStrategy::uniform(3);
    // 3 个均匀权重(实际值由 strategy 内部决定,此处仅验证构造成功)
    let preds = vec![
        make_pred(0.5, 0.3, 0.2, "m1"),
        make_pred(0.5, 0.3, 0.2, "m2"),
        make_pred(0.5, 0.3, 0.2, "m3"),
    ];
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Buy);
}

#[test]
fn test_weighted_vote_size_mismatch_returns_hold() {
    // 2 个预测，3 个权重 → Hold
    let preds = vec![make_pred(0.5, 0.3, 0.2, "m1")];
    let strategy = WeightedVoteStrategy::new(vec![0.5, 0.3, 0.2]).unwrap();
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Hold);
}

#[test]
fn test_weighted_vote_empty_returns_hold() {
    let strategy = WeightedVoteStrategy::uniform(0);
    let result = strategy.combine(&[]);
    assert_eq!(result.action_type, EnsembleAction::Hold);
    assert_eq!(result.confidence, 0.0);
}

#[test]
fn test_weighted_vote_name() {
    let strategy = WeightedVoteStrategy::uniform(1);
    assert_eq!(strategy.name(), "weighted_vote");
}

#[test]
fn test_weighted_vote_zero_weight_ignored() {
    // 模型 2 权重 0 → 仅模型 1 贡献
    let preds = vec![
        make_pred(0.7, 0.2, 0.1, "m1"),
        make_pred(0.1, 0.8, 0.1, "m2"),
    ];
    let strategy = WeightedVoteStrategy::new(vec![1.0, 0.0]).unwrap();
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Buy);
    assert!((result.confidence - 0.7).abs() < 1e-9);
}
