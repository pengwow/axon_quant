//! Trajectory 记录器：记录 ReAct 决策轨迹，落盘为 JSON

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 单个 bar 的轨迹记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryBar {
    /// Bar 索引
    pub bar_id: u64,
    /// 时间戳（毫秒）
    pub ts: i64,
    /// 智能体思考过程
    pub thought: String,
    /// 工具调用（可选）
    pub action: Option<ToolCall>,
    /// 观察结果（可选）
    pub observation: Option<String>,
    /// 当前 bar 奖励
    pub reward: f64,
    /// 累计盈亏
    pub cum_pnl: f64,
    /// 0.11.0: 投票共识记录（多 trader 场景）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consensus: Option<ConsensusRecord>,
}

/// 0.11.0 投票共识记录（嵌入 TrajectoryBar）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusRecord {
    /// 各 trader 投票
    pub votes: Vec<crate::swarm::consensus::AgentVote>,
    /// 聚合结果
    pub aggregated: crate::swarm::consensus::AggregatedVote,
    /// Risk 审核
    pub risk_verdict: crate::swarm::consensus::RiskVerdict,
    /// 最终动作
    pub final_action: crate::swarm::consensus::TraderAction,
    /// Token 消耗
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<crate::swarm::consensus::TokenUsageSnapshot>,
}

/// 工具调用记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// 工具名称
    pub tool: String,
    /// 工具参数
    pub args: serde_json::Value,
}

/// 轨迹摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectorySummary {
    /// 总盈亏
    pub total_pnl: f64,
    /// 交易次数
    pub trades: u64,
    /// 最终持仓
    pub final_position: f64,
    /// 墙钟时间（秒）
    pub wall_time_s: f64,
    /// 成本（USD）
    pub cost_usd: f64,
}

/// 完整轨迹记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    /// 版本号
    pub version: String,
    /// 运行 ID
    pub run_id: String,
    /// 交易标的
    pub instrument: String,
    /// LLM 提供商
    pub provider: String,
    /// 模型名称
    pub model: String,
    /// 随机种子
    pub seed: u64,
    /// 所有 bar 的轨迹
    pub bars: Vec<TrajectoryBar>,
    /// 轨迹摘要（可选）
    pub summary: Option<TrajectorySummary>,
}

/// Trajectory 记录器
pub struct TrajectoryRecorder {
    trajectory: Trajectory,
}

impl TrajectoryRecorder {
    /// 创建新的记录器
    ///
    /// run_id 由 seed 确定性派生(格式 "run-{seed}"),保证同 seed 两次 flush 结果 byte-equal。
    /// 如需自定义 run_id,使用 `with_run_id` 覆盖。
    pub fn new(seed: u64, instrument: String, provider: String, model: String) -> Self {
        let run_id = format!("run-{}", seed);

        Self {
            trajectory: Trajectory {
                version: "0.11.0".to_string(),
                run_id,
                instrument,
                provider,
                model,
                seed,
                bars: Vec::new(),
                summary: None,
            },
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

    /// 记录一个 consensus 决策 bar（0.11.0 便捷方法）
    pub fn record_consensus(
        &mut self,
        bar_id: u64,
        ts: i64,
        decision: &crate::swarm::consensus::ConsensusDecision,
        reward: f64,
        cum_pnl: f64,
    ) {
        let bar = TrajectoryBar {
            bar_id,
            ts,
            thought: format!("consensus: {}", decision.final_action),
            action: None,
            observation: None,
            reward,
            cum_pnl,
            consensus: Some(ConsensusRecord {
                votes: decision.votes.clone(),
                aggregated: decision.aggregated.clone(),
                risk_verdict: decision.risk_verdict.clone(),
                final_action: decision.final_action,
                token_usage: decision.token_usage.clone(),
            }),
        };
        self.trajectory.bars.push(bar);
    }

    /// 设置摘要信息
    pub fn set_summary(&mut self, summary: TrajectorySummary) {
        self.trajectory.summary = Some(summary);
    }

    /// 落盘到 JSON 文件
    pub fn flush(&self, path: &Path) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(&self.trajectory).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// 从 JSON 文件加载轨迹（重放用）
    pub fn replay(path: &Path) -> Result<Self, std::io::Error> {
        let json = std::fs::read_to_string(path)?;
        let trajectory: Trajectory = serde_json::from_str(&json).map_err(std::io::Error::other)?;

        Ok(Self { trajectory })
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
        assert_eq!(recorder.trajectory().version, "0.11.0");
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
            consensus: None,
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
            consensus: None,
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
            consensus: None,
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
                consensus: None,
            });
            recorder2.record(TrajectoryBar {
                bar_id: i as u64,
                ts: 1234567890 + i as i64 * 1000,
                thought: format!("bar {}", i),
                action: None,
                observation: None,
                reward: 0.0,
                cum_pnl: 0.0,
                consensus: None,
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
