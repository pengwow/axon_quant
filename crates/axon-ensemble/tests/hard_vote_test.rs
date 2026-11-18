//! HardVoteStrategy 单元测试

use axon_ensemble::{
    Action, ActionProbabilities, EnsembleAction, HardVoteStrategy, ModelPrediction, ModelType,
    VotingStrategy,
};

fn make_prediction(action_type: EnsembleAction, name: &str) -> ModelPrediction {
    ModelPrediction {
        model_name: name.to_string(),
        model_type: ModelType::RuleBased,
        action: Action {
            action_type,
            symbol: Some("BTC".to_string()),
            quantity: Some(1.0),
            confidence: 0.7,
        },
        confidence: 0.7,
        action_probs: ActionProbabilities::new(0.7, 0.1, 0.2),
    }
}

#[test]
fn test_hard_vote_majority_wins() {
    // 3 个 Buy, 1 个 Sell → Buy 胜
    let preds = vec![
        make_prediction(EnsembleAction::Buy, "m1"),
        make_prediction(EnsembleAction::Buy, "m2"),
        make_prediction(EnsembleAction::Buy, "m3"),
        make_prediction(EnsembleAction::Sell, "m4"),
    ];
    let strategy = HardVoteStrategy;
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Buy);
}

#[test]
fn test_hard_vote_confidence_is_proportion() {
    // 4 Buy, 1 Sell → 置信度 = 4/5 = 0.8
    let preds = vec![
        make_prediction(EnsembleAction::Buy, "m1"),
        make_prediction(EnsembleAction::Buy, "m2"),
        make_prediction(EnsembleAction::Buy, "m3"),
        make_prediction(EnsembleAction::Buy, "m4"),
        make_prediction(EnsembleAction::Sell, "m5"),
    ];
    let strategy = HardVoteStrategy;
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Buy);
    assert!(
        (result.confidence - 0.8).abs() < 1e-9,
        "期望 0.8，实际 {}",
        result.confidence
    );
}

#[test]
fn test_hard_vote_tie_returns_hold() {
    // 1 Buy, 1 Sell, 1 Hold → 都是 1 票 → 选 Hold
    let preds = vec![
        make_prediction(EnsembleAction::Buy, "m1"),
        make_prediction(EnsembleAction::Sell, "m2"),
        make_prediction(EnsembleAction::Hold, "m3"),
    ];
    let strategy = HardVoteStrategy;
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Hold);
}

#[test]
fn test_hard_vote_empty_returns_hold() {
    let strategy = HardVoteStrategy;
    let result = strategy.combine(&[]);
    assert_eq!(result.action_type, EnsembleAction::Hold);
    assert_eq!(result.confidence, 0.0);
}

#[test]
fn test_hard_vote_preserves_symbol_and_quantity() {
    let preds = vec![
        make_prediction(EnsembleAction::Sell, "m1"),
        make_prediction(EnsembleAction::Sell, "m2"),
    ];
    let strategy = HardVoteStrategy;
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Sell);
    assert_eq!(result.symbol.as_deref(), Some("BTC"));
    assert_eq!(result.quantity, Some(1.0));
}

#[test]
fn test_hard_vote_name() {
    let strategy = HardVoteStrategy;
    assert_eq!(strategy.name(), "hard_vote");
}

#[test]
fn test_hard_vote_unanimous_confidence_is_one() {
    let preds = vec![
        make_prediction(EnsembleAction::Buy, "m1"),
        make_prediction(EnsembleAction::Buy, "m2"),
    ];
    let strategy = HardVoteStrategy;
    let result = strategy.combine(&preds);
    assert!((result.confidence - 1.0).abs() < 1e-9);
}
