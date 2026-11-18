//! 集成管理器
//!
//! 统一管理多个模型，提供统一预测接口和多样性度量。

use crate::traits::{Ensemble, Policy, VotingStrategy};
use crate::types::{Action, EnsembleAction, ModelPrediction, ModelWeight, Observation};
use std::time::{SystemTime, UNIX_EPOCH};

/// 集成管理器：统一管理所有模型和投票策略
pub struct EnsembleManager {
    /// 当前使用的投票策略
    strategy: Box<dyn VotingStrategy>,
    /// 所有已注册模型
    models: Vec<Box<dyn Policy>>,
    /// 模型权重（按注册顺序）
    weights: Vec<f64>,
    /// 预测历史：(timestamp, predictions, final_action)
    history: Vec<HistoryRecord>,
}

/// 单条历史记录
#[derive(Debug, Clone)]
pub struct HistoryRecord {
    pub timestamp: u64,
    pub predictions: Vec<ModelPrediction>,
    pub final_action: Action,
}

impl EnsembleManager {
    /// 构造集成管理器
    pub fn new(strategy: Box<dyn VotingStrategy>) -> Self {
        Self {
            strategy,
            models: Vec::new(),
            weights: Vec::new(),
            history: Vec::new(),
        }
    }

    /// 注册一个模型，权重默认均匀分配
    pub fn register_model(&mut self, model: Box<dyn Policy>) {
        self.models.push(model);
        let n = self.models.len();
        self.weights = vec![1.0 / n as f64; n];
    }

    /// 设置权重
    pub fn set_weights(&mut self, weights: Vec<f64>) {
        assert_eq!(
            weights.len(),
            self.models.len(),
            "权重数量必须与模型数量一致"
        );
        self.weights = weights;
    }

    /// 获取权重（带模型名）
    pub fn get_weights(&self) -> Vec<ModelWeight> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.models
            .iter()
            .zip(&self.weights)
            .map(|(m, &w)| ModelWeight {
                model_name: m.name().to_string(),
                weight: w,
                last_updated: now,
            })
            .collect()
    }

    /// 收集所有模型的预测
    fn collect_predictions(&self, observation: &Observation) -> Vec<ModelPrediction> {
        self.models
            .iter()
            .map(|m| ModelPrediction {
                model_name: m.name().to_string(),
                model_type: m.model_type(),
                action: m.predict(observation),
                confidence: m.predict(observation).confidence,
                action_probs: m.action_probs(observation),
            })
            .collect()
    }

    /// 组合预测
    pub fn predict(&mut self, observation: &Observation, timestamp: u64) -> Action {
        let predictions = self.collect_predictions(observation);
        let final_action = self.strategy.combine(&predictions);

        self.history.push(HistoryRecord {
            timestamp,
            predictions,
            final_action: final_action.clone(),
        });

        final_action
    }

    /// 转发到投票策略的 update_weights（用于动态调整）
    pub fn update_weights(&mut self, performances: &[f64]) {
        if performances.len() == self.weights.len() {
            self.weights = performances.to_vec();
        }
    }

    /// 计算模型多样性（预测差异度）
    ///
    /// 多样性 = 不一致的模型对数 / 总模型对数
    /// 多样性 = 1.0 表示完全分歧，0.0 表示完全一致
    pub fn compute_diversity(&self, observations: &[Observation]) -> f64 {
        if observations.is_empty() || self.models.len() < 2 {
            return 0.0;
        }

        let mut disagreements = 0usize;
        let mut total = 0usize;

        for obs in observations {
            let predictions: Vec<EnsembleAction> = self
                .models
                .iter()
                .map(|m| m.predict(obs).action_type)
                .collect();

            let first = predictions[0];
            for &pred in &predictions[1..] {
                total += 1;
                if pred != first {
                    disagreements += 1;
                }
            }
        }

        if total == 0 {
            0.0
        } else {
            disagreements as f64 / total as f64
        }
    }

    /// 历史长度
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// 模型数量
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    /// 访问预测历史
    pub fn history(&self) -> &[HistoryRecord] {
        &self.history
    }

    /// 访问投票策略
    pub fn strategy_name(&self) -> &str {
        self.strategy.name()
    }
}

impl Ensemble for EnsembleManager {
    fn predict(&self, observation: &Observation) -> Action {
        // 注意：Ensemble trait 要求 &self，但 Manager 的 predict 需要 &mut self 来记录历史
        // 这里仅用于 trait 一致性；需要历史时直接调用 predict(observation, timestamp)
        let predictions = self.collect_predictions(observation);
        self.strategy.combine(&predictions)
    }

    fn update_weights(&mut self, performances: &[f64]) {
        // 转发到内部的 update_weights
        self.update_weights(performances);
    }
}
