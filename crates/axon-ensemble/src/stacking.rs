//! 堆叠集成（Stacking）
//!
//! 用元学习器（线性 + softmax）组合多个基模型的预测。

use crate::traits::{Ensemble, Policy};
use crate::types::{Action, ActionProbabilities, EnsembleAction, ModelPrediction, Observation};

/// 元模型（线性层 + softmax）
#[derive(Debug, Clone)]
pub struct MetaModel {
    /// 权重矩阵：\[n_features, n_actions\]
    pub weights: Vec<Vec<f64>>,
    /// 偏置：\[n_actions\]
    pub bias: Vec<f64>,
}

impl MetaModel {
    /// 构造元模型
    ///
    /// - `n_features` 输入特征维度
    /// - `n_actions` 输出维度（典型为 3：buy/sell/hold）
    pub fn new(n_features: usize, n_actions: usize) -> Self {
        let weights = vec![vec![0.0; n_features]; n_actions];
        let bias = vec![0.0; n_actions];
        Self { weights, bias }
    }

    /// 加载指定权重和偏置
    pub fn with_weights(weights: Vec<Vec<f64>>, bias: Vec<f64>) -> Self {
        Self { weights, bias }
    }

    /// 线性 + softmax 前向传播
    pub fn predict(&self, features: &[f64]) -> ActionProbabilities {
        let mut output = self.bias.clone();

        // output[j] = bias[j] + sum_i features[i] * weights[j][i]
        for (j, weight_row) in self.weights.iter().enumerate() {
            for (i, &f) in features.iter().enumerate() {
                if i < weight_row.len() {
                    output[j] += f * weight_row[i];
                }
            }
        }

        // softmax
        let max_val = output.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let exp_vals: Vec<f64> = output.iter().map(|x| (x - max_val).exp()).collect();
        let sum_exp: f64 = exp_vals.iter().sum();
        let probs: Vec<f64> = exp_vals.iter().map(|x| x / sum_exp).collect();

        ActionProbabilities {
            buy: probs.first().copied().unwrap_or(1.0 / 3.0),
            sell: probs.get(1).copied().unwrap_or(1.0 / 3.0),
            hold: probs.get(2).copied().unwrap_or(1.0 / 3.0),
        }
    }
}

/// 堆叠集成
pub struct StackingEnsemble {
    base_models: Vec<Box<dyn Policy>>,
    meta_model: MetaModel,
}

impl StackingEnsemble {
    /// 构造堆叠集成
    pub fn new(base_models: Vec<Box<dyn Policy>>, meta_model: MetaModel) -> Self {
        Self {
            base_models,
            meta_model,
        }
    }

    /// 构造堆叠特征：基模型预测概率 + 置信度 + 观测特征（降采样）
    fn build_stacking_features(
        &self,
        observation: &Observation,
        base_predictions: &[ModelPrediction],
    ) -> Vec<f64> {
        let mut features = Vec::new();

        // 各模型的预测概率（每个 3 维：buy/sell/hold）
        for pred in base_predictions {
            let v = pred.action_probs.to_vec();
            features.extend(v);
        }

        // 各模型的置信度
        for pred in base_predictions {
            features.push(pred.confidence);
        }

        // 原始观测特征（合并所有特征字段）
        let mut all_features = Vec::new();
        all_features.extend(observation.market_features.iter().copied());
        all_features.extend(observation.technical_indicators.iter().copied());
        all_features.extend(observation.time_features.iter().copied());

        // 降采样到最多 32 维
        let target_dim = 32.min(all_features.len().max(1));
        if !all_features.is_empty() {
            let step = (all_features.len() / target_dim).max(1);
            let sampled: Vec<f64> = all_features
                .iter()
                .step_by(step)
                .take(target_dim)
                .copied()
                .collect();
            features.extend(sampled);
        }

        features
    }

    /// 获取基模型数量
    pub fn base_model_count(&self) -> usize {
        self.base_models.len()
    }

    /// 访问元模型
    pub fn meta_model(&self) -> &MetaModel {
        &self.meta_model
    }
}

impl Ensemble for StackingEnsemble {
    fn predict(&self, observation: &Observation) -> Action {
        if self.base_models.is_empty() {
            return Action {
                action_type: EnsembleAction::Hold,
                symbol: None,
                quantity: None,
                confidence: 0.0,
            };
        }

        // 获取基模型预测
        let base_predictions: Vec<ModelPrediction> = self
            .base_models
            .iter()
            .map(|m| ModelPrediction {
                model_name: m.name().to_string(),
                model_type: m.model_type(),
                action: m.predict(observation),
                confidence: m.predict(observation).confidence,
                action_probs: m.action_probs(observation),
            })
            .collect();

        // 构造堆叠特征并元模型推理
        let features = self.build_stacking_features(observation, &base_predictions);
        let probs = self.meta_model.predict(&features);

        let action_type = if probs.buy > probs.sell && probs.buy > probs.hold {
            EnsembleAction::Buy
        } else if probs.sell > probs.buy && probs.sell > probs.hold {
            EnsembleAction::Sell
        } else {
            EnsembleAction::Hold
        };

        let first = &base_predictions[0].action;
        Action {
            action_type,
            symbol: first.symbol.clone(),
            quantity: first.quantity,
            confidence: probs.buy.max(probs.sell).max(probs.hold),
        }
    }

    fn update_weights(&mut self, _performances: &[f64]) {
        // 堆叠模型通过元模型训练更新，不直接调整权重
    }
}
