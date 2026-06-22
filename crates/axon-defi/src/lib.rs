//! AXON DeFi 链上交易
//!
//! EVM 链适配器、Uniswap V3、MEV-Share、LayerZero 跨链。

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod bridge;
pub mod dex;
pub mod error;
pub mod evm;
pub mod mev;
pub mod types;

/// DeFi 模块版本
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
