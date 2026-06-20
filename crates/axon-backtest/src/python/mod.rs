//! `axon-backtest` Python 绑定模块(Stage 2)
//!
//! 把 `axon-backtest` 的 L1/L2/L3 撮合引擎 + `BacktestEngine` 主循环 +
//! 已有 `ImpactedMatchingEngine` 全部通过 PyO3 暴露到 Python,形成
//! `axon_quant.backtest` 子模块。
//!
//! # 子模块
//!
//! - [`error`]: `MatchingError` / `MatchingL3Error` → `PyBacktestError(PyException)` 统一异常
//! - `types`: dict 协议辅助函数(参考 `impact/python.rs`,后续 Task 3 填充)
//! - `matching_l1`: `L1MatchingEngine` 暴露(后续 Task 4)
//! - `matching_l2`: `L2MatchingEngine` + `MatchingStats`(后续 Task 5)
//! - `matching_l3`: `MultiAssetMatchingEngine` + `DarkPool` + `Auction`(后续 Task 6)
//! - `impact`: 迁移 `ImpactedMatchingEngine` + 新增 `ImpactModel` 抽象(Task 7 完成)
//! - `engine`: `BacktestEngine` + `RunResult` + `RunStats`(Task 8 完成)
//!
//! # 设计决策
//!
//! - 与 `axon-data` 一致:`BacktestError` 继承 builtin `PyException`,
//!   **不**继承 `axon_python::error::AxonError`(避免 `axon-backtest`
//!   反向依赖 `axon-python` 导致 cargo 循环)。Python 端 thin wrapper
//!   可在 `python/axon_quant/backtest.py` 中通过 `__bases__` 注入"伪
//!   继承",但 Rust 侧不做硬依赖。详见
//!   `.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md`
//!   §3.1.6 + `crates/axon-data/src/python/error.rs` 注释。
//! - 子模块按需逐步添加(本文件**只**注册已实现的子模块),保证
//!   每个 Task 收口后 cargo build / cargo test 始终通过。

#![cfg(feature = "python")]

// 已实现:error 子模块(Task 1)
// Task 3 完成后:`types` 子模块,提供 dict 协议共用工具。
// Task 4 完成后:`matching_l1` 子模块,暴露 L1MatchingEngine。
// Task 5 完成后:`matching_l2` 子模块,暴露 L2MatchingEngine + OrderBookEntry。
// Task 6 完成后:`matching_l3` 子模块,暴露 MultiAssetMatchingEngine + 配套类型。
// Task 7 完成后:`impact` 子模块,迁移 ImpactedMatchingEngine + 新增 ImpactModel 抽象。
// Task 8 完成后:`engine` 子模块,暴露 BacktestEngine + RunResult + RunStats。

pub mod engine;
pub mod error;
pub mod impact;
pub mod matching_l1;
pub mod matching_l2;
pub mod matching_l3;
pub mod types;

use pyo3::prelude::*;

/// 在父模块(`_native.backtest`)下注册全部已实现的子模块。
///
/// 调用方:`crates/axon-python/src/lib.rs::axon_python::_native` 中先
/// 创建 `PyModule::new("backtest")` 再调 `axon_backtest::python::register_module`.
///
/// **重要:** 这里**不**再 `PyModule::new("backtest").add_submodule` 嵌套,
/// 而是直接把类注册到 `parent`(`_native.backtest`)上 —— 与
/// `axon-data::python::register_module` 做法一致,原因:
/// `_native` 是单文件 cdylib 扩展,Python 端只能通过属性访问
/// (`_native.backtest.BacktestError`),无法走 import 路径
/// (`_native.backtest.error.BacktestError`)。
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    // 直接把子模块的类注册到 `parent`(即 `_native.backtest`),
    // 让 Python 端可以 `from axon_quant._native import backtest; backtest.BacktestError`。
    error::register(parent)?;
    types::register(parent)?;
    matching_l1::register(parent)?;
    matching_l2::register(parent)?;
    matching_l3::register(parent)?;
    impact::register(parent)?;
    engine::register(parent)?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    /// `register_module` 函数签名稳定:接受 `&Bound<'_, PyModule>`,返回 `PyResult<()>`
    /// 这里只验证编译期签名;运行时验证在 `python/tests/test_backtest_e2e.py`(Task 13)。
    #[test]
    fn register_module_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register_module;
    }
}
