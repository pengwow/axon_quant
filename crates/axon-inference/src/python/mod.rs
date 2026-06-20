//! axon-inference Python 绑定模块(Stage 6)
//!
//! 子模块:
//! - [`error`][]: `InferenceError` → `PyInferenceError(PyException)`(避免 cargo 循环)
//! - [`config`][]: `ModelConfig` / `InferenceBackend` / `Device` / `Observation` / `Action` / `ActionType` / `BatchConfig` / `InferenceStats`
//! - [`engine`][]: `InferenceEngine`(Onnx / Candle / Tch 后端)
//! - [`pipeline`][]: `BatchInferencePipeline` + `ModelHotReloader`
//!
//! 设计约束:
//! - `InferenceError` 继承 builtin `PyException` 而非 `AxonError`,避免
//!   `axon-inference` 反向依赖 `axon-python` 造成 cargo 循环
//!   (同 backtest / risk / oms / exchange,详见 design spec §3.1.6)。
//! - 后端实现:Onnx / Candle 走 `cfg(feature = "...")` 门控,Python 端只暴露已启用的后端。
//! - 推理是 CPU 同步计算,无异步依赖,不需要 `block_on` 包装。

#![cfg(feature = "python")]

pub mod config;
pub mod engine;
pub mod error;
pub mod pipeline;

use pyo3::prelude::*;

/// 把 `inference` 子模块注册到父模块(`_native`)下。
///
/// 与 Stage 1-5 保持一致:不嵌套 `add_submodule`,所有 pyclass 扁平
/// 注册到 `parent`(`_native.inference`),cdylib 模式下 Python 端
/// 仅可通过属性访问(`from axon_quant._native.inference import InferenceEngine`)。
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(parent)?;
    config::register(parent)?;
    engine::register(parent)?;
    pipeline::register(parent)?;
    Ok(())
}
