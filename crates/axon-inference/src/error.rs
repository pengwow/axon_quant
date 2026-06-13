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
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_batch_size: 32,
            collect_timeout_us: 500,
            num_workers: 2,
            prealloc_buffer_size: 64,
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
    pub action_type: ActionType,
    pub confidence: f32,
    pub target_position: f32,
    pub model_id: String,
    pub inference_time_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Hold,
    Buy,
    Sell,
    ReduceLong,
    ReduceShort,
}

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
