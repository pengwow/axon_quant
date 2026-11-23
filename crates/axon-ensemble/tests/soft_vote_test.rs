//! SoftVoteStrategy 单元测试

use axon_ensemble::{
    Action, ActionProbabilities, EnsembleAction, ModelPrediction, ModelType, SoftVoteStrategy,
    VotingStrategy,
};

fn make_pred(buy: f64, sell: f64, hold: f64, name: &str) -> ModelPrediction {
    ModelPrediction {
        model_name: name.to_string(),
        model_type: ModelType::PPO,
        action: Action {
            action_type: EnsembleAction::Hold,
            symbol: Some("ETH".to_string()),
            quantity: Some(2.0),
            confidence: 0.5,
        },
        confidence: 0.5,
        action_probs: ActionProbabilities::new(buy, sell, hold),
    }
}

#[test]
fn test_soft_vote_averages_probabilities() {
    // 模型 1: (0.6, 0.2, 0.2)
    // 模型 2: (0.4, 0.4, 0.2)
    // 平均:   (0.5, 0.3, 0.2) → Buy 胜
    let preds = vec![
        make_pred(0.6, 0.2, 0.2, "m1"),
        make_pred(0.4, 0.4, 0.2, "m2"),
    ];
    let strategy = SoftVoteStrategy;
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Buy);
    assert!((result.confidence - 0.5).abs() < 1e-9);
}

#[test]
fn test_soft_vote_picks_highest_avg() {
    // 3 模型: (0.3, 0.5, 0.2), (0.3, 0.5, 0.2), (0.3, 0.5, 0.2)
    // 平均 sell = 0.5，最大
    let preds = vec![
        make_pred(0.3, 0.5, 0.2, "m1"),
        make_pred(0.3, 0.5, 0.2, "m2"),
        make_pred(0.3, 0.5, 0.2, "m3"),
    ];
    let strategy = SoftVoteStrategy;
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Sell);
}

#[test]
fn test_soft_vote_empty_returns_hold() {
    let strategy = SoftVoteStrategy;
    let result = strategy.combine(&[]);
    assert_eq!(result.action_type, EnsembleAction::Hold);
    assert_eq!(result.confidence, 0.0);
}

#[test]
fn test_soft_vote_preserves_symbol() {
    let preds = vec![make_pred(0.5, 0.3, 0.2, "m1")];
    let strategy = SoftVoteStrategy;
    let result = strategy.combine(&preds);
    assert_eq!(result.symbol.as_deref(), Some("ETH"));
    assert_eq!(result.quantity, Some(2.0));
}

#[test]
fn test_soft_vote_name() {
    assert_eq!(SoftVoteStrategy.name(), "soft_vote");
}

#[test]
fn test_soft_vote_handles_unnormalized_inputs() {
    // 输入未归一化 (2, 4, 4) 内部会归一化 → (0.2, 0.4, 0.4)
    let preds = vec![make_pred(2.0, 4.0, 4.0, "m1")];
    let strategy = SoftVoteStrategy;
    let result = strategy.combine(&preds);
    assert_eq!(result.action_type, EnsembleAction::Sell);
    assert!((result.confidence - 0.4).abs() < 1e-9);
}
