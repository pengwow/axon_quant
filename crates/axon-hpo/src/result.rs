//! HPO 运行结果

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::StudyConfig;
use crate::pareto::ParetoPoint;
use crate::trial::TrialResult;

/// HPO 完整运行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HPOResult {
    /// study 配置
    pub study_config: StudyConfig,
    /// 最佳 trial
    pub best_trial: Option<TrialResult>,
    /// 所有 trials
    pub all_trials: Vec<TrialResult>,
    /// 参数重要性（参数名 → 重要性分数 0~1）
    pub param_importances: HashMap<String, f64>,
    /// 多目标时的 Pareto 前沿
    pub pareto_front: Option<Vec<ParetoPoint>>,
    /// 总耗时（毫秒）
    pub elapsed_ms: u64,
}

impl HPOResult {
    /// 创建空结果
    pub fn empty(study_config: StudyConfig) -> Self {
        Self {
            study_config,
            best_trial: None,
            all_trials: Vec::new(),
            param_importances: HashMap::new(),
            pareto_front: None,
            elapsed_ms: 0,
        }
    }

    /// 完成 trial 数
    pub fn n_complete(&self) -> usize {
        self.all_trials
            .iter()
            .filter(|t| t.state.is_complete())
            .count()
    }

    /// 完成 trial 数（按 state 过滤）
    pub fn n_by_state(&self, state: crate::trial::TrialState) -> usize {
        self.all_trials.iter().filter(|t| t.state == state).count()
    }
}
