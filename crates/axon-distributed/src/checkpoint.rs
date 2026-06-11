//! Checkpoint 与训练指标

use serde::{Deserialize, Serialize};

/// 单步训练指标
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepMetrics {
    /// 训练步数
    pub step: usize,
    /// 平均 episode 奖励
    pub episode_reward_mean: f64,
    /// 平均 episode 长度
    pub episode_len_mean: f64,
    /// 策略损失
    pub policy_loss: f64,
    /// 价值损失
    pub value_loss: f64,
    /// 熵
    pub entropy: f64,
    /// 每秒帧数
    pub fps: f64,
}

/// Checkpoint 元数据
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointMetadata {
    /// 训练迭代数
    pub iteration: usize,
    /// 时间戳（毫秒）
    pub timestamp_ms: u64,
    /// step 指标历史
    pub metrics_history: Vec<StepMetrics>,
}

/// 训练状态快照（用于 checkpoint & restore）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingCheckpoint {
    /// 训练迭代数
    pub iteration: usize,
    /// 序列化的 policy 权重
    pub policy_state: Vec<u8>,
    /// 序列化的 optimizer 状态
    pub optimizer_state: Vec<u8>,
    /// 随机数状态
    pub rng_state: Vec<u8>,
    /// step 指标历史
    pub metrics_history: Vec<StepMetrics>,
    /// 时间戳（毫秒）
    pub timestamp_ms: u64,
}

impl TrainingCheckpoint {
    /// 创建新 checkpoint
    pub fn new(
        iteration: usize,
        policy_state: Vec<u8>,
        optimizer_state: Vec<u8>,
        rng_state: Vec<u8>,
    ) -> Self {
        Self {
            iteration,
            policy_state,
            optimizer_state,
            rng_state,
            metrics_history: Vec::new(),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }

    /// 估算 checkpoint 大小（字节）
    pub fn size_bytes(&self) -> usize {
        self.policy_state.len()
            + self.optimizer_state.len()
            + self.rng_state.len()
            + self.metrics_history.len() * std::mem::size_of::<StepMetrics>()
    }

    /// 序列化为 JSON
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// 从 JSON 反序列化
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// 添加 step 指标
    pub fn add_metrics(&mut self, metrics: StepMetrics) {
        self.metrics_history.push(metrics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_training_checkpoint_new() {
        let ckpt = TrainingCheckpoint::new(100, vec![0u8; 1024], vec![0u8; 512], vec![0u8; 256]);
        assert_eq!(ckpt.iteration, 100);
        assert_eq!(ckpt.size_bytes(), 1024 + 512 + 256);
    }

    #[test]
    fn test_training_checkpoint_json_roundtrip() {
        let mut ckpt = TrainingCheckpoint::new(50, vec![1, 2, 3], vec![4, 5], vec![6, 7, 8, 9]);
        ckpt.add_metrics(StepMetrics {
            step: 50,
            episode_reward_mean: 1.5,
            episode_len_mean: 100.0,
            policy_loss: 0.01,
            value_loss: 0.05,
            entropy: 0.5,
            fps: 1000.0,
        });
        let json = ckpt.to_json().expect("serialize");
        let restored = TrainingCheckpoint::from_json(&json).expect("deserialize");
        assert_eq!(restored.iteration, 50);
        assert_eq!(restored.metrics_history.len(), 1);
        assert!((restored.metrics_history[0].episode_reward_mean - 1.5).abs() < 1e-9);
    }

    #[test]
    fn test_checkpoint_metadata() {
        let meta = CheckpointMetadata {
            iteration: 10,
            timestamp_ms: 12345,
            metrics_history: vec![],
        };
        let json = serde_json::to_string(&meta).expect("serialize");
        let restored: CheckpointMetadata = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.iteration, 10);
        assert_eq!(restored.timestamp_ms, 12345);
    }

    #[test]
    fn test_step_metrics_equality() {
        let m1 = StepMetrics {
            step: 1,
            episode_reward_mean: 1.0,
            episode_len_mean: 50.0,
            policy_loss: 0.1,
            value_loss: 0.2,
            entropy: 0.3,
            fps: 500.0,
        };
        let m2 = m1.clone();
        assert_eq!(m1, m2);
    }
}
