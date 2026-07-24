//! 0.9.0 D1.4b:多 leg 推理结果类型
//!
//! 扩展现有 `Action` 离散 5 类语义,支持多 leg 连续目标仓位(0.9.0 demo 2-3 leg)。
use serde::{Deserialize, Serialize};

/// 多 leg 推理结果(0.9.0 D1.4b 新增)
///
/// `target_positions[i]` 是 leg i 的目标仓位(归一化 [-1, 1])
/// `len(target_positions) == leg 数`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiLegAction {
    /// 各 leg 目标仓位(归一化 [-1, 1]),长度 = leg 数
    pub target_positions: Vec<f32>,
    /// 模型 ID(便于多模型 hot-reload 场景区分)
    pub model_id: String,
    /// 推理耗时(微秒,记录用于性能监控)
    pub inference_time_us: u64,
}
