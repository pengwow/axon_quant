//! axon-llm PyO3 模块入口
//!
//! 暴露 `LLMBackend` / `LLMMessage` 类 + `make_backend` 函数。
//! 典型用法:Python 端用 dict 传 LLMConfig,Rust 端校验后构造 backend。
//!
//! ## 设计说明
//!
//! - `make_backend(config_dict)`:从 Python dict 构造 `LLMBackend`,
//!   内部用 `LLMConfig::from_dict` 解析 + `OpenAICompatConfig::from_llm_config` 构造。
//! - `LLMBackend.chat([...])`:同步 chat,内部把 async complete 桥到 sync。
//! - `LLMMessage`:Python 端 DTO,内部转 Rust `Message`。

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::backends::{OpenAICompatBackend, OpenAICompatConfig};
use crate::config::LLMConfig;

mod agent;
use agent::{PyReActAgent, PyTool};

mod backend;
use backend::{PyLLMBackend, PyMessage};

mod ollama;
use ollama::{PyOllamaBackend, make_ollama_backend};

pub mod trading;

pub mod trajectory;

pub mod swarm;

mod helpers;
use helpers::{pythonize, type_name};

// ═══════════════════════════════════════════════════════════
// TokenMeter Python 绑定
// ═══════════════════════════════════════════════════════════

/// Python 端 Token 计量器
///
/// 可独立使用（手动 record）或由 VotingOrchestrator 内部持有。
#[pyclass(name = "TokenMeter")]
#[derive(Clone)]
pub struct PyTokenMeter {
    inner: Arc<crate::meter::TokenMeter>,
}

#[pymethods]
impl PyTokenMeter {
    /// 构造 TokenMeter
    ///
    /// Args:
    ///     model: 模型名（用于 cost 估算）
    ///     max_input_tokens: 最大 input token（可选）
    ///     max_output_tokens: 最大 output token（可选）
    ///     warn_threshold: 预警阈值 0.0-1.0（默认 0.8）
    #[new]
    #[pyo3(signature = (model, max_input_tokens=None, max_output_tokens=None, warn_threshold=None))]
    fn new(
        model: String,
        max_input_tokens: Option<u64>,
        max_output_tokens: Option<u64>,
        warn_threshold: Option<f32>,
    ) -> Self {
        use crate::backends::MockBackend;
        let budget = match (max_input_tokens, max_output_tokens) {
            (Some(mi), Some(mo)) => Some(crate::meter::TokenBudget {
                max_input_tokens: mi,
                max_output_tokens: mo,
                warn_threshold: warn_threshold.unwrap_or(0.8),
            }),
            _ => None,
        };
        // 内部用空 MockBackend 占位；实际使用时由 orchestrator 注入真实 backend
        let meter = crate::meter::TokenMeter::new(Box::new(MockBackend::new()), budget, model);
        Self {
            inner: Arc::new(meter),
        }
    }

    /// 获取当前汇总报告
    fn report<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let r = self.inner.report();
        let d = PyDict::new(py);
        d.set_item("input_tokens", r.input_tokens)?;
        d.set_item("output_tokens", r.output_tokens)?;
        d.set_item("total_tokens", r.total_tokens)?;
        d.set_item("call_count", r.call_count)?;
        d.set_item("estimated_cost_usd", r.estimated_cost_usd)?;
        d.set_item("over_budget", r.over_budget)?;
        Ok(d)
    }

    /// 重置计数器
    fn reset(&self) {
        self.inner.reset();
    }

    fn __repr__(&self) -> String {
        let r = self.inner.report();
        format!(
            "TokenMeter(calls={}, tokens={}, cost=${:.4})",
            r.call_count, r.total_tokens, r.estimated_cost_usd
        )
    }
}

/// Python 端 `LLMBackend` 的构造函数
///
/// `config` 是 dict,字段:
///   - `backends`: list[dict],每个 dict 包含 base_url/api_key/model/max_tokens/temperature/timeout_secs
///   - `retry`: dict{max_retries, initial_backoff_ms, max_backoff_ms}(可选)
///   - `explain`: dict{record_decisions, store_path}(可选)
///
/// 返回 `LLMBackend` 实例。
#[pyfunction]
fn make_backend(py: Python<'_>, config: &Bound<'_, PyDict>) -> PyResult<PyLLMBackend> {
    // 1. Python dict → serde_json::Value
    let json_value = pythonize(py, config.as_any())?;

    // 2. 转为 HashMap<String, Value>(供 LLMConfig::from_dict)
    let map: std::collections::HashMap<String, serde_json::Value> = match json_value {
        serde_json::Value::Object(m) => m.into_iter().collect(),
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "config must be a dict, got {}",
                type_name(&other)
            )));
        }
    };

    // 3. 解析为 LLMConfig(内部会 validate)
    let cfg = LLMConfig::from_dict(map)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

    // 4. 构造 OpenAICompatConfig(取第一个 backend)
    let compat = OpenAICompatConfig::from_llm_config(&cfg, 0)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    let backend = OpenAICompatBackend::new(compat);

    // 5. 创建独占 tokio runtime
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    Ok(PyLLMBackend {
        inner: Arc::new(Mutex::new(backend)),
        runtime: Arc::new(runtime),
    })
}

/// `axon_llm` pymodule 入口
///
/// 由 `#[pymodule]` 宏标记,可被 Python 直接 `import axon_llm` 加载
/// (要求 `crate-type = ["cdylib"]` 且 build 时启用 `python` feature)。
///
/// 同时也供 `axon-python` crate 通过 `axon_llm::python::axon_llm` 调用,
/// 把它作为子模块挂载到统一的 `_native.llm` 命名空间下。
#[pymodule]
pub fn axon_llm(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(make_backend, m)?)?;
    m.add_function(wrap_pyfunction!(make_ollama_backend, m)?)?;
    m.add_class::<PyLLMBackend>()?;
    m.add_class::<PyOllamaBackend>()?;
    m.add_class::<PyMessage>()?;
    m.add_class::<PyTokenMeter>()?;
    m.add_class::<PyReActAgent>()?;
    m.add_class::<PyTool>()?;
    let trading_submodule = PyModule::new(m.py(), "trading")?;
    trading::register_trading_module(&trading_submodule)?;
    m.add_submodule(&trading_submodule)?;
    let trajectory_submodule = PyModule::new(m.py(), "trajectory")?;
    trajectory::register_trajectory_module(&trajectory_submodule)?;
    m.add_submodule(&trajectory_submodule)?;
    let swarm_submodule = PyModule::new(m.py(), "swarm")?;
    swarm::register_swarm_module(&swarm_submodule)?;
    m.add_submodule(&swarm_submodule)?;
    Ok(())
}
