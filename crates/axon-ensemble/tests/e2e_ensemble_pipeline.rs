//! 端到端测试:axon-ensemble 集成管理器完整流程
//!
//! ## 4 个测试场景
//!
//! 1. `ensemble_hard_vote_pipeline`:注册 3 个策略 → HardVote → 预测 → 验证投票结果
//! 2. `ensemble_soft_vote_weighted`:SoftVote 加权 → 验证概率归一化
//! 3. `ensemble_dynamic_weight_update`:动态调权 → 验证权重变化
//! 4. `ensemble_diversity_score`:多模型多样性度量
//!
//! 运行:`cargo test -p axon-ensemble --test e2e_ensemble_pipeline`

use axon_ensemble::{
    Action, ActionProbabilities, ActionType, EnsembleManager, HardVoteStrategy, Observation,
    Policy, SoftVoteStrategy, WeightedVoteStrategy,
};

// ── helpers ────────────────────────────────────────────────────────────

struct FixedPolicy {
    name: String,
    action_type: ActionType,
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
    fn model_type(&self) -> axon_ensemble::ModelType {
        axon_ensemble::ModelType::PPO
    }
    fn action_probs(&self, _observation: &Observation) -> ActionProbabilities {
        match self.action_type {
            ActionType::Buy => ActionProbabilities::new(0.8, 0.1, 0.1),
            ActionType::Sell => ActionProbabilities::new(0.1, 0.8, 0.1),
            ActionType::Hold => ActionProbabilities::new(0.1, 0.1, 0.8),
        }
    }
}

fn buy_policy(name: &str) -> Box<dyn Policy> {
    Box::new(FixedPolicy {
        name: name.to_string(),
        action_type: ActionType::Buy,
    })
}

fn sell_policy(name: &str) -> Box<dyn Policy> {
    Box::new(FixedPolicy {
        name: name.to_string(),
        action_type: ActionType::Sell,
    })
}

fn hold_policy(name: &str) -> Box<dyn Policy> {
    Box::new(FixedPolicy {
        name: name.to_string(),
        action_type: ActionType::Hold,
    })
}

fn test_observation() -> Observation {
    Observation::default()
}

// ── 1. HardVote: 3 个 Buy → 投票结果为 Buy ───────────────────────────

#[test]
fn ensemble_hard_vote_pipeline() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(buy_policy("m1"));
    manager.register_model(buy_policy("m2"));
    manager.register_model(buy_policy("m3"));

    assert_eq!(manager.model_count(), 3);

    let action = manager.predict(&test_observation(), 1000);
    assert_eq!(action.action_type, ActionType::Buy);
    assert_eq!(manager.history_len(), 1);
}

// ── 2. SoftVote: 2 Buy + 1 Sell → Buy 胜出 ───────────────────────────

#[test]
fn ensemble_soft_vote_weighted() {
    let mut manager = EnsembleManager::new(Box::new(SoftVoteStrategy));
    manager.register_model(buy_policy("m1"));
    manager.register_model(buy_policy("m2"));
    manager.register_model(sell_policy("m3"));

    let action = manager.predict(&test_observation(), 2000);
    // 2 Buy vs 1 Sell → Buy 胜出
    assert_eq!(action.action_type, ActionType::Buy);
    assert!(action.confidence > 0.0);
}

// ── 3. 动态调权: 注册 → 预测 → 验证历史记录 ──────────────────────────

#[test]
fn ensemble_dynamic_weight_update() {
    let mut manager = EnsembleManager::new(Box::new(WeightedVoteStrategy::uniform(2)));
    manager.register_model(buy_policy("m1"));
    manager.register_model(sell_policy("m2"));

    // 预测并记录历史
    let _action1 = manager.predict(&test_observation(), 3000);
    assert_eq!(manager.history_len(), 1);

    // 更新权重（manager 内部）
    manager.set_weights(vec![0.1, 0.9]);

    // 再次预测
    let _action2 = manager.predict(&test_observation(), 4000);
    assert_eq!(manager.history_len(), 2);

    // 验证权重可读
    let weights = manager.get_weights();
    assert_eq!(weights.len(), 2);
    assert!((weights[0].weight - 0.1).abs() < 1e-9);
    assert!((weights[1].weight - 0.9).abs() < 1e-9);
}

// ── 4. 多样性: 3 个不同策略 → diversity > 0 ──────────────────────────

#[test]
fn ensemble_diversity_score() {
    let mut manager = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager.register_model(buy_policy("m1"));
    manager.register_model(sell_policy("m2"));
    manager.register_model(hold_policy("m3"));

    let diversity = manager.compute_diversity(&[test_observation()]);
    assert!(
        diversity > 0.0,
        "3 个不同策略应有非零多样性,实为 {diversity}"
    );
    assert!(diversity <= 1.0);

    // 完全一致 → diversity = 0
    let mut manager2 = EnsembleManager::new(Box::new(HardVoteStrategy));
    manager2.register_model(buy_policy("m1"));
    manager2.register_model(buy_policy("m2"));
    let diversity2 = manager2.compute_diversity(&[test_observation()]);
    assert_eq!(diversity2, 0.0);
}
