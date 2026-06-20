//! `axon-oms` Python 绑定模块(Stage 4)
//!
//! 把 `axon-oms` 的 `OrderManager` / `Order` / `Side` / `OrderType` /
//! `OrderStatus` / `Portfolio` / `Position` 通过 PyO3 暴露到 Python,
//! 形成 `axon_quant.oms` 子模块。
//!
//! # 子模块
//!
//! - `error`: `OmsError` → `PyOmsError(PyException)`
//! - `decimal`: `rust_decimal::Decimal` ↔ Python `decimal.Decimal` 桥
//! - `types`: `Side` / `OrderType` / `OrderStatus` / `Order`
//! - `manager`: `OrderManager`
//! - `portfolio`: `Portfolio` / `Position`(借 `OrderManager.snapshot_balance/positions` 桥接)
//!
//! # 设计决策
//!
//! - 与 `axon-backtest` / `axon-risk` 一致:`OmsError` 继承 builtin
//!   `PyException`,**不**继承 `axon_python::error::AxonError`(避免
//!   `axon-oms` 反向依赖 `axon-python` 导致 cargo 循环)。Python 端
//!   thin wrapper 可在 `python/axon_quant/oms.py` 中通过 `__bases__`
//!   注入"伪继承",但 Rust 侧不做硬依赖。详见
//!   `crates/axon-backtest/src/python/error.rs` 注释。
//!
//! - `Order::new` 不接受 `idempotency_key`,需要 `with_idempotency_key`
//!   链式;Python 端 `PyOrder` 在 `__new__` 接受 `idempotency_key` 关键字,
//!   内部用 `with_idempotency_key` 链上,保持 Rust 内部 API 一致。
//!
//! - `OrderManager` 没有 `batch_submit` 方法,Rust API 仅支持单个
//!   `submit` / `cancel` / `update_status`;Python 端 `batch_submit`
//!   是基于 `submit` 循环的语义糖,保持 Rust 内部锁模式不被绕过。
//!
//! - 子模块按需逐步添加(本文件**只**注册已实现的子模块),保证
//!   每个 Task 收口后 cargo build / cargo test 始终通过。

#![cfg(feature = "python")]

pub mod decimal;
pub mod error;
pub mod manager;
pub mod portfolio;
pub mod types;

use pyo3::prelude::*;

/// 在父模块(`_native.oms`)下注册全部已实现的子模块。
///
/// 调用方:`crates/axon-python/src/lib.rs::_native` 中先创建
/// `PyModule::new("oms")` 再调 `axon_oms::python::register_module`.
///
/// **重要:** 这里**不**再 `PyModule::new("oms").add_submodule` 嵌套,
/// 而是直接把类注册到 `parent`(`_native.oms`)上 —— 与
/// `axon-risk::python::register_module` 做法一致,原因:
/// `_native` 是单文件 cdylib 扩展,Python 端只能通过属性访问
/// (`_native.oms.OmsError`),无法走 import 路径
/// (`_native.oms.error.OmsError`)。
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(parent)?;
    types::register(parent)?;
    manager::register(parent)?;
    portfolio::register(parent)?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    /// `register_module` 函数签名稳定:接受 `&Bound<'_, PyModule>`,返回 `PyResult<()>`
    /// 这里只验证编译期签名;运行时验证在 `python/tests/test_oms_e2e.py`(Task 10)。
    #[test]
    fn register_module_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register_module;
    }
}
