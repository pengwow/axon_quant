//! 观测空间模块
//!
//! 定义 RL 智能体感知市场状态的方式，包括特征工程、归一化、窗口聚合。
//!
//! 提供 Gymnasium 兼容的 `BoxSpace` 接口（Python 侧可零拷贝导出 numpy 数组）。
//!
//! # 子模块
//!
//! - [`types`]：核心类型（Observation / BoxSpace / MarketState / FeatureConfig / ...）
//! - [`normalizer`]：归一化器（ZScore / MinMax / Robust / Noop）与运行时统计量
//! - [`buffer`]：环形缓冲区，维护最近 N 个 tick
//! - [`spec`]：观测组装规格，训练与推理共用的唯一实现
//! - [`space`]：默认观测空间实现，委托 [`spec`] 完成组装
//! - [`error`]：错误类型与统一 Result

#![deny(unsafe_code)]

pub mod buffer;
pub mod error;
pub mod normalizer;
pub mod space;
pub mod spec;
pub mod types;

#[cfg(test)]
mod tests;

pub use buffer::TickBuffer;
pub use error::{ObservationError, ObservationResult, validate_observation_space};
pub use normalizer::{
    MinMaxNormalizer, NoopNormalizer, Normalizer, RobustNormalizer, ZScoreNormalizer,
    make_normalizer,
};
pub use space::DefaultObservationSpace;
pub use spec::{DEFAULT_SPEC_VERSION, NormalizerConfig, ObservationInput, ObservationSpec};
pub use types::{
    AggregationType, BoxSpace, DType, FeatureConfig, FeatureSource, MarketState, Observation,
    TimeFeature,
};

// 重新导出 normalizer.rs 中通过 `pub use` 暴露的 RunningStats
pub use normalizer::RunningStats;
