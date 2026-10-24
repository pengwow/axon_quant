//! 交易环境模块
//!
//! 暴露 Gymnasium 兼容的 `TradingEnv`，整合观测、动作、奖励与执行器。

pub mod action_decoder;
pub mod config;
pub mod cost;
pub mod error;
pub mod executor;
pub mod trading_env;
pub mod types;

pub use action_decoder::ActionDecoder;
pub use config::EnvConfig;
pub use cost::CostModel;
pub use error::{EnvError, EnvResult};
pub use executor::Executor;
pub use trading_env::{StepResult, TradingEnv};
pub use types::{EnvInfo, ExecutionResult, MarketBar};
