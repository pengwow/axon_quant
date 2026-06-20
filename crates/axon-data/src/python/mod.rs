//! `axon-data` Python 绑定模块入口(Stage 1)
//!
//! 暴露到 `axon_quant._native.data` 子模块。
//! 子模块清单(随 Stage 1 进度逐步填充):
//! - [`error`]: `DataError` → `PyDataError(PyException)` 转换
//! - [`types`]: `Frequency` / `DataRequest`(Task 4)
//! - [`traits`]: `DataSource` Python 端抽象(Task 5)
//! - [`sources`]: `MockSource` 暴露(Task 6)
//! - [`dataset`]: `Dataset` / `SchemaField` 包装(Task 7)
//! - [`service`]: `DataService` / `CacheControl` 包装(Task 8)
//!
//! 异常基类说明:
//! 本模块**不依赖** `axon-python`(避免 cargo 循环依赖),
//! `DataError` 直接继承 builtin `PyException`;Python 端用
//! `except Exception` 统一捕获,或用 `except _native.data.DataError` 精确捕获。
//! 详见 `.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md` §3.1.6。

#![cfg(feature = "python")]

// 后续 Task 8 完成后会扩展为:
// pub mod traits;
pub mod dataset;
pub mod error;
pub mod service;
pub mod sources;
pub mod types;

use pyo3::prelude::*;

/// 在父模块(`_native.data`)下注册全部子模块。
///
/// 调用方:`crates/axon-python/src/lib.rs` 的 `#[pymodule] _native` 中
/// 先创建 `PyModule::new("data")`,再调 `axon_data::python::register_module`。
///
/// **重要:** 这里**不**再 `PyModule::new("data")` 再 `add_submodule`,
/// 而是直接把类注册到 `parent`(`_native.data`)上 —— 因为 `_native` 是单文件
/// cdylib 扩展,没有真正的 `__path__`,Python 端只能通过属性访问
/// (`_native.data.CacheControl`)拿到符号,无法走 import 路径
/// (`_native.data.dataset.CacheControl`)。这与 `axon_llm` 的做法保持一致。
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    // 直接把类注册到 `parent`(即 `_native.data`),
    // 让 Python 端可以 `from axon_quant._native import data; data.CacheControl`。
    error::register(parent)?;
    types::register(parent)?;
    sources::register(parent)?;
    dataset::register(parent)?;
    service::register(parent)?;
    Ok(())
}
