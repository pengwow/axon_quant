//! AXON 超参数优化
//!
//! 提供完整的 HPO 工具链：Optuna 集成、搜索空间定义、剪枝策略、
//! 多目标优化、Pareto 前沿与超体积计算。
//!
//! # 模块规划
//!
//! | 模块 | 说明 |
//! |------|------|
//! | [`config`] | HPO/Study/Sampler/Pruner 配置 |
//! | [`search_space`] | 搜索空间定义（Uniform / LogUniform / Choice / ...）|
//! | [`trial`] | Trial 结果与状态 |
//! | [`result`] | HPO 运行结果与 Pareto 前沿 |
//! | [`pareto`] | Pareto 前沿计算与超体积指标 |
//! | [`error`] | 统一错误类型 |
//! | `python` | PyO3 绑定（feature = `python`） |

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod config;
pub mod error;
pub mod pareto;
pub mod result;
pub mod search_space;
pub mod trial;

#[cfg(feature = "python")]
pub mod python;

pub use config::{
    HPOConfig, ObjectiveConfig, ObjectiveDef, PrunerConfig, PrunerType, SamplerConfig, SamplerType,
    StudyConfig, StudyDirection,
};
pub use error::{HPOError, HPOResult};
pub use pareto::{ParetoFront, ParetoPoint, compute_hypervolume, compute_pareto_front, dominates};
pub use result::HPOResult as HPOOutput;
pub use search_space::SearchSpaceDef;
pub use trial::{TrialResult, TrialState};
