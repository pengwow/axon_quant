use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceBackend {
    Onnx,
    Tch,
    Candle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Device {
    Cpu,
    Cuda(u32),
    Metal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub path: PathBuf,
    pub backend: InferenceBackend,
    pub device: Device,
    pub input_shape: [usize; 3],
    pub output_dim: usize,
    pub fp16: bool,
    pub num_threads: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchConfig {
    pub max_batch_size: usize,
    pub collect_timeout_us: u64,
    pub num_workers: usize,
    pub prealloc_buffer_size: usize,
    /// CPU 亲和性:启动时绑定的 core id 列表(空 = 不绑核)
    #[serde(default)]
    pub collect_cpu_cores: Vec<u32>,
    /// CUDA 设备亲和性:启动时绑定的 GPU device id(None = 不绑)
    #[serde(default)]
    pub collect_gpu_device_id: Option<u32>,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_batch_size: 32,
            collect_timeout_us: 500,
            num_workers: 2,
            prealloc_buffer_size: 64,
            collect_cpu_cores: Vec::new(),
            collect_gpu_device_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub symbol: String,
    pub timestamp_ns: u64,
    pub features: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub action_type: TradeAction,
    pub confidence: f32,
    pub target_position: f32,
    pub model_id: String,
    pub inference_time_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeAction {
    Hold,
    Buy,
    Sell,
    ReduceLong,
    ReduceShort,
}

/// [`TradeAction`] 的历史名（deprecated 别名，1 个 minor 后移除）。
///
/// 拆名背景：`ActionType` 曾同时指代三种不同语义（inference 的交易动作、
/// RL 的动作空间类别、ensemble 的集成动作）。本类型是 inference 的"交易
/// 动作"，故更名 `TradeAction`。注意 Python 绑定对外的类名仍为
/// "ActionType"（见 `python/config.rs` 的 pyclass name），Python API 不受影响。
pub type ActionType = TradeAction;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceStats {
    pub total_inferences: u64,
    pub total_batch_inferences: u64,
    pub avg_latency_us: f64,
    pub p99_latency_us: f64,
    pub hot_reloads: u64,
    pub errors: u64,
}

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("model file not found: {path}")]
    ModelNotFound { path: PathBuf },

    #[error("model load failed: {reason}")]
    ModelLoadFailed { reason: String },

    #[error("model not loaded: call backend.load(path) before infer")]
    ModelNotLoaded,

    #[error("inference failed: {reason}")]
    InferenceFailed { reason: String },

    #[error("dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },

    #[error("device unavailable: {device:?}")]
    DeviceUnavailable { device: Device },

    #[error("hot reload failed: {reason}")]
    HotReloadFailed { reason: String },

    #[error("onnx error: {0}")]
    Onnx(String),

    #[error("tch error: {0}")]
    Tch(String),

    #[error("candle error: {0}")]
    Candle(String),
}
