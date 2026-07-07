//! EVM 链模块
//!
//! 0.3.0 P0:在原有 `chain` 模块基础上新增 `provider` + `signer` + `erc20` 模块,
//! 封装 alloy 真链交互。

pub mod chain;
pub mod erc20;
pub mod multicall;
pub mod provider;
pub mod signer;
