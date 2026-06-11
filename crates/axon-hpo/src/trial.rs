//! Trial 结果与状态

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Trial 状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialState {
    /// 正在运行
    Running,
    /// 完整完成
    Complete,
    /// 被剪枝
    Pruned,
    /// 失败
    Fail,
}

impl TrialState {
    /// 是否完成（成功 / 失败 / 剪枝都算"已结束"）
    pub fn is_finished(&self) -> bool {
        !matches!(self, TrialState::Running)
    }

    /// 是否成功
    pub fn is_complete(&self) -> bool {
        matches!(self, TrialState::Complete)
    }

    /// 转换为字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            TrialState::Running => "running",
            TrialState::Complete => "complete",
            TrialState::Pruned => "pruned",
            TrialState::Fail => "fail",
        }
    }
}

/// 单次 Trial 的结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialResult {
    /// trial ID
    pub trial_id: i32,
    /// 试验参数
    pub params: HashMap<String, serde_json::Value>,
    /// 目标值（单目标：1 个；多目标：N 个）
    pub values: Vec<f64>,
    /// 状态
    pub state: TrialState,
    /// 耗时（毫秒）
    pub duration_ms: u64,
    /// 中间值（用于早停）：(step, value)
    #[serde(default)]
    pub intermediate_values: Vec<(usize, f64)>,
}

impl TrialResult {
    /// 创建新 trial 结果
    pub fn new(
        trial_id: i32,
        params: HashMap<String, serde_json::Value>,
        values: Vec<f64>,
    ) -> Self {
        Self {
            trial_id,
            params,
            values,
            state: TrialState::Complete,
            duration_ms: 0,
            intermediate_values: Vec::new(),
        }
    }

    /// 设置状态
    pub fn with_state(mut self, state: TrialState) -> Self {
        self.state = state;
        self
    }

    /// 设置耗时
    pub fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = duration_ms;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trial_state_is_finished() {
        assert!(!TrialState::Running.is_finished());
        assert!(TrialState::Complete.is_finished());
        assert!(TrialState::Pruned.is_finished());
        assert!(TrialState::Fail.is_finished());
    }

    #[test]
    fn test_trial_state_is_complete() {
        assert!(!TrialState::Running.is_complete());
        assert!(TrialState::Complete.is_complete());
        assert!(!TrialState::Pruned.is_complete());
    }

    #[test]
    fn test_trial_result_builder() {
        let mut params = HashMap::new();
        params.insert("lr".into(), serde_json::json!(0.001));
        let r = TrialResult::new(1, params, vec![0.5])
            .with_state(TrialState::Complete)
            .with_duration(1000);
        assert_eq!(r.trial_id, 1);
        assert_eq!(r.values, vec![0.5]);
        assert_eq!(r.duration_ms, 1000);
        assert!(r.state.is_complete());
    }
}
