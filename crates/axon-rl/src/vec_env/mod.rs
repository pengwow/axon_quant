//! 向量化环境模块
//!
//! 管理 N 个并行运行的 `TradingEnv` 实例，提高 RL 采样效率。
//!
//! ## 子模块
//!
//! | 子模块 | 说明 |
//! |--------|------|
//! | [`error`] | `VecEnvError` 错误类型 |
//! | [`stats`] | `VecEnvStatistics` 统计信息 |
//! | [`factory`] | `EnvFactory` trait + `BasicEnvFactory` 工厂 |
//! | [`sync`] | `SyncVecEnv` 同步（顺序）版本 |
//! | [`async_env`] | `AsyncVecEnv` 异步（std::thread 并行）版本 |
//!
//! ## 选择指南
//!
//! - 单元测试 / 调试 / 小规模（< 4 envs）：使用 [`SyncVecEnv`]
//! - 大规模并行采样：使用 [`AsyncVecEnv`]

pub mod async_env;
pub mod error;
pub mod factory;
pub mod stats;
pub mod sync;

#[cfg(test)]
mod tests;

pub use async_env::AsyncVecEnv;
pub use error::{VecEnvError, VecEnvResult};
pub use factory::{BasicEnvFactory, EnvFactory};
pub use stats::VecEnvStatistics;
pub use sync::SyncVecEnv;
