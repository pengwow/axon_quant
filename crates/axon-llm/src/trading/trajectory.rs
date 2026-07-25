//! Trajectory 记录器：记录 ReAct 决策轨迹，落盘为 JSON

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 单个 bar 的轨迹记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryBar {
    pub bar_id: u64,
    pub ts: i64,
    pub thought: String,
    pub action: Option<ToolCall>,
    pub observation: Option<String>,
    pub reward: f64,
    pub cum_pnl: f64,
}

/// 工具调用记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    pub args: serde_json::Value,
}

/// 轨迹摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectorySummary {
    pub total_pnl: f64,
    pub trades: u64,
    pub final_position: f64,
    pub wall_time_s: f64,
    pub cost_usd: f64,
}

/// 完整轨迹记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    pub version: String,
    pub run_id: String,
    pub instrument: String,
    pub provider: String,
    pub model: String,
    pub seed: u64,
    pub bars: Vec<TrajectoryBar>,
    pub summary: Option<TrajectorySummary>,
}

/// Trajectory 记录器
pub struct TrajectoryRecorder {
    trajectory: Trajectory,
    start_ts: i64,
}

impl TrajectoryRecorder {
    /// 创建新的记录器
    pub fn new(seed: u64, instrument: String, provider: String, model: String) -> Self {
        let run_id = Uuid::new_v4().to_string();
        let start_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        Self {
            trajectory: Trajectory {
                version: "0.10.0".to_string(),
                run_id,
                instrument,
                provider,
                model,
                seed,
                bars: Vec::new(),
                summary: None,
            },
            start_ts,
        }
    }

    /// 设置 run_id（用于测试和确定性重放）
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.trajectory.run_id = run_id.into();
        self
    }

    /// 记录一个 bar 的轨迹
    pub fn record(&mut self, bar: TrajectoryBar) {
        self.trajectory.bars.push(bar);
    }

    /// 设置摘要信息
    pub fn set_summary(&mut self, summary: TrajectorySummary) {
        self.trajectory.summary = Some(summary);
    }

    /// 落盘到 JSON 文件
    pub fn flush(&self, path: &Path) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(&self.trajectory)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }

    /// 从 JSON 文件加载轨迹（重放用）
    pub fn replay(path: &Path) -> Result<Self, std::io::Error> {
        let json = std::fs::read_to_string(path)?;
        let trajectory: Trajectory = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        Ok(Self {
            trajectory,
            start_ts: 0,
        })
    }

    /// 获取当前轨迹数据
    pub fn trajectory(&self) -> &Trajectory {
        &self.trajectory
    }

    /// 获取当前轨迹数据的可变引用
    pub fn trajectory_mut(&mut self) -> &mut Trajectory {
        &mut self.trajectory
    }

    /// 获取运行 ID
    pub fn run_id(&self) -> &str {
        &self.trajectory.run_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn trajectory_recorder_new_has_correct_version() {
        let recorder = TrajectoryRecorder::new(42, "BTC-USDT".into(), "mock".into(), "test".into());
        assert_eq!(recorder.trajectory().version, "0.10.0");
        assert!(!recorder.run_id().is_empty());
        assert_eq!(recorder.trajectory().seed, 42);
    }

    #[test]
    fn trajectory_recorder_record_adds_bar() {
        let mut recorder =
            TrajectoryRecorder::new(42, "BTC-USDT".into(), "mock".into(), "test".into());

        let bar = TrajectoryBar {
            bar_id: 0,
            ts: 1234567890,
            thought: "test".into(),
            action: None,
            observation: None,
            reward: 0.0,
            cum_pnl: 0.0,
        };

        recorder.record(bar);
        assert_eq!(recorder.trajectory().bars.len(), 1);
    }

    #[test]
    fn trajectory_recorder_flush_and_replay_consistent() {
        let mut recorder =
            TrajectoryRecorder::new(42, "BTC-USDT".into(), "mock".into(), "test".into());

        recorder.record(TrajectoryBar {
            bar_id: 0,
            ts: 1234567890,
            thought: "test".into(),
            action: None,
            observation: None,
            reward: 0.0,
            cum_pnl: 0.0,
        });

        let temp_path = PathBuf::from("/tmp/test_trajectory.json");
        recorder.flush(&temp_path).unwrap();

        let replayed = TrajectoryRecorder::replay(&temp_path).unwrap();
        assert_eq!(replayed.trajectory().bars.len(), 1);
        assert_eq!(replayed.trajectory().seed, 42);

        std::fs::remove_file(&temp_path).ok();
    }

    #[test]
    fn trajectory_record_has_tool_call() {
        let mut recorder =
            TrajectoryRecorder::new(42, "BTC-USDT".into(), "mock".into(), "test".into());

        let bar = TrajectoryBar {
            bar_id: 0,
            ts: 1234567890,
            thought: "buy".into(),
            action: Some(ToolCall {
                tool: "place_order".into(),
                args: serde_json::json!({"symbol": "BTC-USDT", "side": "Buy"}),
            }),
            observation: Some("ack".into()),
            reward: 1.0,
            cum_pnl: 1.0,
        };

        recorder.record(bar);
        let recorded_bar = &recorder.trajectory().bars[0];
        assert_eq!(recorded_bar.action.as_ref().unwrap().tool, "place_order");
        assert_eq!(recorded_bar.reward, 1.0);
    }

    #[test]
    fn trajectory_deterministic_same_seed_byte_equal() {
        let run_id = "deterministic-test-run";
        let mut recorder1 =
            TrajectoryRecorder::new(42, "BTC-USDT".into(), "mock".into(), "test".into())
                .with_run_id(run_id);
        let mut recorder2 =
            TrajectoryRecorder::new(42, "BTC-USDT".into(), "mock".into(), "test".into())
                .with_run_id(run_id);

        for i in 0..5 {
            recorder1.record(TrajectoryBar {
                bar_id: i as u64,
                ts: 1234567890 + i as i64 * 1000,
                thought: format!("bar {}", i),
                action: None,
                observation: None,
                reward: 0.0,
                cum_pnl: 0.0,
            });
            recorder2.record(TrajectoryBar {
                bar_id: i as u64,
                ts: 1234567890 + i as i64 * 1000,
                thought: format!("bar {}", i),
                action: None,
                observation: None,
                reward: 0.0,
                cum_pnl: 0.0,
            });
        }

        let temp_path1 = PathBuf::from("/tmp/test_trajectory_det1.json");
        let temp_path2 = PathBuf::from("/tmp/test_trajectory_det2.json");
        recorder1.flush(&temp_path1).unwrap();
        recorder2.flush(&temp_path2).unwrap();

        let content1 = std::fs::read(&temp_path1).unwrap();
        let content2 = std::fs::read(&temp_path2).unwrap();

        assert_eq!(
            content1, content2,
            "same seed should produce identical trajectory files"
        );

        std::fs::remove_file(&temp_path1).ok();
        std::fs::remove_file(&temp_path2).ok();
    }
}
