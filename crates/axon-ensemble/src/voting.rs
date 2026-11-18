//! 投票策略实现
//!
//! - `HardVoteStrategy` — 多数表决
//! - `SoftVoteStrategy` — 概率平均
//! - `WeightedVoteStrategy` — 按权重加权

use std::collections::HashMap;

use crate::traits::VotingStrategy;
use crate::types::{Action, EnsembleAction, ModelPrediction};

/// 硬投票：多数表决
#[derive(Debug, Clone, Copy, Default)]
pub struct HardVoteStrategy;

impl VotingStrategy for HardVoteStrategy {
    fn combine(&self, predictions: &[ModelPrediction]) -> Action {
        if predictions.is_empty() {
            return Action {
                action_type: EnsembleAction::Hold,
                symbol: None,
                quantity: None,
                confidence: 0.0,
            };
        }

        // 统计每个 action_type 的票数
        let mut votes: HashMap<EnsembleAction, usize> = HashMap::new();
        for pred in predictions {
            *votes.entry(pred.action.action_type).or_insert(0) += 1;
        }

        // 选取得票最多的动作；平票时按 Hold > Sell > Buy 优先
        let priority = |a: &EnsembleAction| match a {
            EnsembleAction::Hold => 2,
            EnsembleAction::Sell => 1,
            EnsembleAction::Buy => 0,
        };
        let mut entries: Vec<(EnsembleAction, usize)> = votes.into_iter().collect();
        entries
            .sort_by(|(a1, c1), (a2, c2)| c2.cmp(c1).then_with(|| priority(a2).cmp(&priority(a1))));
        let (majority_action, winning_votes) = entries
            .into_iter()
            .next()
            .unwrap_or((EnsembleAction::Hold, 0));

        // 置信度 = 得票比例
        let confidence = winning_votes as f64 / predictions.len() as f64;

        // 保留第一个预测的 symbol/quantity（业务约定）
        let first = &predictions[0].action;

        Action {
            action_type: majority_action,
            symbol: first.symbol.clone(),
            quantity: first.quantity,
            confidence,
        }
    }

    fn name(&self) -> &str {
        "hard_vote"
    }
}

/// 软投票：动作概率平均
#[derive(Debug, Clone, Copy, Default)]
pub struct SoftVoteStrategy;

impl VotingStrategy for SoftVoteStrategy {
    fn combine(&self, predictions: &[ModelPrediction]) -> Action {
        if predictions.is_empty() {
            return Action {
                action_type: EnsembleAction::Hold,
                symbol: None,
                quantity: None,
                confidence: 0.0,
            };
        }

        let n = predictions.len() as f64;
        let avg_buy: f64 = predictions.iter().map(|p| p.action_probs.buy).sum::<f64>() / n;
        let avg_sell: f64 = predictions.iter().map(|p| p.action_probs.sell).sum::<f64>() / n;
        let avg_hold: f64 = predictions.iter().map(|p| p.action_probs.hold).sum::<f64>() / n;

        // 选择平均概率最高的动作
        let action_type = argmax_action(avg_buy, avg_sell, avg_hold);
        let confidence = avg_buy.max(avg_sell).max(avg_hold);

        let first = &predictions[0].action;
        Action {
            action_type,
            symbol: first.symbol.clone(),
            quantity: first.quantity,
            confidence,
        }
    }

    fn name(&self) -> &str {
        "soft_vote"
    }
}

/// 加权投票：按权重加权平均
#[derive(Debug, Clone)]
pub struct WeightedVoteStrategy {
    weights: Vec<f64>,
}

impl WeightedVoteStrategy {
    /// 构造加权投票策略，权重和必须为 1
    pub fn new(weights: Vec<f64>) -> Result<Self, crate::EnsembleError> {
        let sum: f64 = weights.iter().sum();
        if (sum - 1.0).abs() > crate::WEIGHT_TOLERANCE {
            return Err(crate::EnsembleError::InvalidWeights { sum });
        }
        Ok(Self { weights })
    }

    /// 用均匀权重构造
    pub fn uniform(n: usize) -> Self {
        let w = if n == 0 { 0.0 } else { 1.0 / n as f64 };
        Self {
            weights: vec![w; n],
        }
    }
}

impl VotingStrategy for WeightedVoteStrategy {
    fn combine(&self, predictions: &[ModelPrediction]) -> Action {
        if predictions.is_empty() || self.weights.len() != predictions.len() {
            return Action {
                action_type: EnsembleAction::Hold,
                symbol: None,
                quantity: None,
                confidence: 0.0,
            };
        }

        let mut wb = 0.0;
        let mut ws = 0.0;
        let mut wh = 0.0;

        for (pred, w) in predictions.iter().zip(&self.weights) {
            wb += pred.action_probs.buy * w;
            ws += pred.action_probs.sell * w;
            wh += pred.action_probs.hold * w;
        }

        let action_type = argmax_action(wb, ws, wh);
        let confidence = wb.max(ws).max(wh);

        let first = &predictions[0].action;
        Action {
            action_type,
            symbol: first.symbol.clone(),
            quantity: first.quantity,
            confidence,
        }
    }

    fn name(&self) -> &str {
        "weighted_vote"
    }
}

/// 选择 (buy, sell, hold) 中最大值对应的 action
fn argmax_action(buy: f64, sell: f64, hold: f64) -> EnsembleAction {
    if buy >= sell && buy >= hold {
        EnsembleAction::Buy
    } else if sell >= buy && sell >= hold {
        EnsembleAction::Sell
    } else {
        EnsembleAction::Hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_argmax_action() {
        assert_eq!(argmax_action(0.5, 0.3, 0.2), EnsembleAction::Buy);
        assert_eq!(argmax_action(0.2, 0.5, 0.3), EnsembleAction::Sell);
        assert_eq!(argmax_action(0.3, 0.3, 0.4), EnsembleAction::Hold);
    }
}
