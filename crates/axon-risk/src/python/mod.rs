//! `axon-risk` Python 绑定模块(Stage 3)
//!
//! 把 `axon-risk` 的 `DefaultRiskEngine` / `CircuitBreaker` /
//! `RiskConfig` / `RiskMetrics` 通过 PyO3 暴露到 Python,形成
//! `axon_quant.risk` 子模块。
//!
//! # 子模块
//!
//! - `circuit_breaker`: `CircuitBreaker`
//! - `config`: `RiskConfig` 配置 dataclass
//! - `engine`: `DefaultRiskEngine` + `RiskResult` 枚举 + dict→Order/Portfolio 桥
//! - `error`: `RiskError` → `PyRiskError(PyException)`
//! - `metrics`: `RiskMetrics` dict 转换 helper
//!
//! # 设计决策
//!
//! - 与 `axon-backtest` 一致:`RiskError` 继承 builtin `PyException`,
//!   **不**继承 `axon_python::error::AxonError`(避免 `axon-risk`
//!   反向依赖 `axon-python` 导致 cargo 循环)。Python 端 thin wrapper
//!   可在 `python/axon_quant/risk.py` 中通过 `__bases__` 注入"伪
//!   继承",但 Rust 侧不做硬依赖。详见
//!   `crates/axon-backtest/src/python/error.rs` 注释。
//!
//! - `RiskEventHandler` Stage 3 **不**暴露给 Python:它是 axon-core
//!   的 `EventHandler` trait,需构造 `Arc<dyn RiskEngine>` +
//!   `Arc<RwLock<Portfolio>>`,Stage 4 暴露 OMS 时一起做更自然。
//!
//! - 子模块按需逐步添加(本文件**只**注册已实现的子模块),保证
//!   每个 Task 收口后 cargo build / cargo test 始终通过。

#![cfg(feature = "python")]

pub mod circuit_breaker;
pub mod config;
pub mod engine;
pub mod error;
pub mod metrics;

use pyo3::prelude::*;

/// 在父模块(`_native.risk`)下注册全部已实现的子模块。
///
/// 调用方:`crates/axon-python/src/lib.rs::_native` 中先创建
/// `PyModule::new("risk")` 再调 `axon_risk::python::register_module`.
///
/// **重要:** 这里**不**再 `PyModule::new("risk").add_submodule` 嵌套,
/// 而是直接把类注册到 `parent`(`_native.risk`)上 —— 与
/// `axon-backtest::python::register_module` 做法一致,原因:
/// `_native` 是单文件 cdylib 扩展,Python 端只能通过属性访问
/// (`_native.risk.RiskConfig`),无法走 import 路径
/// (`_native.risk.config.RiskConfig`)。
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(parent)?;
    config::register(parent)?;
    engine::register(parent)?;
    circuit_breaker::register(parent)?;
    metrics::register(parent)?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    /// `register_module` 函数签名稳定:接受 `&Bound<'_, PyModule>`,返回 `PyResult<()>`
    /// 这里只验证编译期签名;运行时验证在 `python/tests/test_risk_e2e.py`(Task 8)。
    #[test]
    fn register_module_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register_module;
    }
}
