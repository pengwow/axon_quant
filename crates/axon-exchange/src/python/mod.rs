//! axon-exchange Python 绑定模块(Stage 5)
//!
//! 子模块:
//! - [`error`][]: `ExchangeError` → `PyExchangeError(PyException)`(避免 cargo 循环)
//! - [`config`][]: `ExchangeConfig` / `ExchangeId` / `RateLimitConfig` / `ReconnectConfig`
//! - [`binance`][]: `BinanceAdapter` 暴露(sync `block_on` 包装)
//! - [`okx`][]: `OkxAdapter` 暴露(sync `block_on` 包装)
//! - [`lifecycle`][]: `OrderLifecycleManager`
//! - [`rate_limiter`][]: `TokenBucketRateLimiter` 状态读取
//!
//! 设计约束:
//! - `ExchangeError` 继承 builtin `PyException` 而非 `AxonError`,避免
//!   `axon-exchange` 反向依赖 `axon-python` 造成 cargo 循环
//!   (同 backtest / risk / oms,详见 design spec §3.1.6)。
//! - 异步 API 在 Rust 端用 `tokio::runtime::Runtime::block_on` 同步包装,
//!   Python 端不需要 asyncio(简化调用模型)。
//! - `api_secret` **不**暴露到 `__repr__`,避免日志泄漏。

#![cfg(feature = "python")]

pub mod binance;
pub mod config;
pub mod error;
pub mod lifecycle;
pub mod okx;
pub mod rate_limiter;

use pyo3::prelude::*;

/// 把 `exchange` 子模块注册到父模块(`_native`)下。
///
/// 与 Stage 1-4 保持一致:不嵌套 `add_submodule`,所有 pyclass 扁平
/// 注册到 `parent`(`_native.exchange`),cdylib 模式下 Python 端
/// 仅可通过属性访问(`from axon_quant._native.exchange import BinanceAdapter`)。
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(parent)?;
    config::register(parent)?;
    binance::register(parent)?;
    okx::register(parent)?;
    lifecycle::register(parent)?;
    rate_limiter::register(parent)?;
    Ok(())
}
