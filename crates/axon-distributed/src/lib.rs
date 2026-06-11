//! AXON 分布式训练
//!
//! 提供 Ray 集群集成 + RLLib 算法执行 + Parameter Server +
//! Checkpoint 容错的完整工具链。
//!
//! # 模块规划
//!
//! | 模块 | 说明 |
//! |------|------|
//! | [`config`] | DistributedConfig + Cluster/Algorithm/Resource/FaultTolerance |
//! | [`actor`] | ActorConfig |
//! | [`param_server`] | ParamServerConfig |
//! | [`checkpoint`] | TrainingCheckpoint + StepMetrics + CheckpointMetadata |
//! | [`error`] | 统一错误类型 |
//! | `python` | PyO3 绑定（feature = `python`） |

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod actor;
pub mod checkpoint;
pub mod config;
pub mod error;
pub mod param_server;

#[cfg(feature = "python")]
pub mod python;

pub use actor::ActorConfig;
pub use checkpoint::{CheckpointMetadata, StepMetrics, TrainingCheckpoint};
pub use config::{
    AlgorithmConfig, ClusterConfig, DistributedConfig, FaultToleranceConfig, ResourceConfig,
};
pub use error::{DistributedError, DistributedResult};
pub use param_server::ParamServerConfig;
