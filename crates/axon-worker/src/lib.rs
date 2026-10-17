//! 策略编排 + 全 Rust 原生回测运行器
//!
//! axon-strategy 定义"策略是什么"，axon-backtest 定义"撮合引擎"，
//! axon-worker 定义"怎么把策略装到引擎上跑一遍"。

pub mod backtest;
pub mod manager;

pub use backtest::*;
pub use manager::*;

#[cfg(feature = "python")]
pub mod python;
