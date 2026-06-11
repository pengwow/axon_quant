//! VecEnv 统计信息

use serde::{Deserialize, Serialize};

/// 向量化环境聚合统计
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VecEnvStatistics {
    /// 环境总数
    pub num_envs: usize,
    /// 每个环境的累计奖励
    pub total_rewards: Vec<f64>,
    /// 每个环境的 step 计数
    pub step_counts: Vec<usize>,
    /// 已 done 的环境数量
    pub done_count: usize,
    /// 是否所有环境都已 done
    pub all_done: bool,
}

impl VecEnvStatistics {
    /// 平均累计奖励
    pub fn mean_reward(&self) -> f64 {
        if self.total_rewards.is_empty() {
            0.0
        } else {
            self.total_rewards.iter().sum::<f64>() / self.total_rewards.len() as f64
        }
    }

    /// 平均步数
    pub fn mean_steps(&self) -> f64 {
        if self.step_counts.is_empty() {
            0.0
        } else {
            self.step_counts.iter().sum::<usize>() as f64 / self.step_counts.len() as f64
        }
    }
}
