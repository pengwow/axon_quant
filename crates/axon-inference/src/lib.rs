//! # axon-inference
//!
//! 推理引擎：ONNX/tch/Candle 后端，批推理管线，模型热更新。
//!
//! ## 核心功能
//!
//! - **多后端**：ONNX Runtime、tch-rs（PyTorch）、Candle（纯 Rust）
//! - **多设备**：CPU、CUDA、Metal
//! - **批推理**：tokio + rayon 异步批处理管线
//! - **热更新**：notify 文件监控 + 原子替换模型
//! - **低延迟**：内存池、CPU 亲和性、FP16 推理
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! use axon_inference::{ModelConfig, InferenceBackend, Device, Observation};
//!
//! // 配置模型
//! let config = ModelConfig {
//!     path: "model.onnx".into(),
//!     backend: InferenceBackend::Onnx,
//!     device: Device::Cpu,
//!     input_shape: [1, 64, 128], // batch, seq_len, features
//!     output_dim: 3,
//!     fp16: false,
//!     num_threads: 4,
//! };
//!
//! // 创建观测数据
//! let observation = Observation {
//!     symbol: "BTC-USDT".into(),
//!     timestamp_ns: 1_000_000_000,
//!     features: vec![0.0f32; 128],
//! };
//! ```
//!
//! ## 支持的后端
//!
//! | 后端 | 特性 | 适用场景 |
//! |------|------|----------|
//! | ONNX | CPU/CUDA/TensorRT | 生产环境 |
//! | tch-rs | PyTorch C++ 后端 | 灵活性高 |
//! | Candle | 纯 Rust 实现 | 无 Python 依赖 |
//!
//! ## 性能目标
//!
//! | 操作 | 目标 |
//! |------|------|
//! | 单次推理（CPU） | < 500µs |
//! | 16 资产批推理 | < 1ms |
//! | 热更新切换 | < 10ms |

pub mod affinity;
pub mod backend;
pub mod engine;
pub mod error;
pub mod hot_reload;
pub mod pipeline;
pub mod types;

#[cfg(feature = "python")]
pub mod python;

pub use engine::InferenceEngine;
pub use error::{
    Action, ActionType, BatchConfig, Device, InferenceBackend, InferenceError, InferenceStats,
    ModelConfig, Observation,
};
pub use hot_reload::ModelHotReloader;
pub use pipeline::batch::BatchInferencePipeline;
pub use types::MultiLegAction;
