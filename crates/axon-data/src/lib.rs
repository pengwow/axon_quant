//! AXON 数据服务
//!
//! 提供统一的市场数据接入、特征计算与缓存能力（Phase 3 P2 骨架）。
//!
//! ## 模块
//!
//! - [`types`]:核心数据类型 `DataRequest` / `Frequency` / `Dataset`
//! - [`error`]:统一错误类型 `DataError`
//! - [`dataset`]:数据集合（内存表示 + 行式迭代）
//! - [`traits`]:数据源抽象 `DataSource` trait
//! - [`sources`]:具体数据源实现（默认仅暴露 `MockSource`,csv/ws feature-gated）
//! - [`pipeline`][]:特征管道（归一化、滑动窗口）骨架
//! - [`cache`]:L1 缓存运维(`CacheControl`)+ L2 mmap 共享缓存(`mmap-cache` feature)
//!
//! ## Feature flag
//!
//! - `csv-source`:启用 `CsvSource`
//! - `ws-source`:启用 `WebSocketSource`(默认关闭)
//! - `mmap-cache`:启用 L2 mmap 共享缓存(memmap2 unsafe API)

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
// 业务模块禁止 unsafe;mmap 子模块单独 `#[allow(unsafe_code)]` 显式放行
// (原因:memmap2 / fs2 内部需要 unsafe,封装在 cache::mmap 内对业务透明)
#![deny(unsafe_code)]

pub mod bar;
pub mod cache;
pub mod dataset;
pub mod error;
pub mod ipc;
pub mod pipeline;
pub mod sources;
pub mod traits;
pub mod types;

// 内部模块
mod service;
// Property-based fuzz tests(仅测试时编译)
#[cfg(test)]
mod fuzz;

// 公开 re-export
pub use cache::CacheControl;
pub use dataset::Dataset;
pub use error::DataError;
pub use pipeline::{FeatureMatrix, FeaturePipeline, Normalizer, ZScoreNormalizer};
pub use service::{CacheStats, DataService};
pub use sources::MockSource;
pub use traits::DataSource;
pub use types::{DataRequest, Frequency};
