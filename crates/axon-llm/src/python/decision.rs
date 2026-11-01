//! SDM 决策层 PyO3 绑定，注册为 `_native.llm.decision`

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_async_runtimes::tokio::future_into_py;

use crate::decision::backend::{DecisionBackend, DecisionError};
use crate::decision::cascade::{
    CascadeConfig, FallbackDecider, RuleFallback, SdmCascadeDecisionLayer,
};
use crate::decision::http::{SdmHttpBackend, SdmHttpConfig};
use crate::decision::replay::ReplayDecisionBackend;
use crate::decision::stub::StubDecisionBackend;
use crate::decision::types::{DecisionRequest, DecisionSource};
use tokio::sync::Mutex as TokioMutex;

use crate::backend::LLMBackend;
use crate::backends::OpenAICompatBackend;
use crate::decision::llm_fallback::parse_llm_content;
use crate::types::Message;

use super::helpers::pythonize;

/// Python 侧决策后端包装（持有 trait 对象，runtime 由全局 `_block_on` 管理）
#[pyclass(name = "DecisionBackend", skip_from_py_object)]
#[derive(Clone)]
pub struct PyDecisionBackend {
    pub(crate) inner: Arc<dyn DecisionBackend>,
}

/// Python 侧规则兜底
#[pyclass(name = "RuleFallback", skip_from_py_object)]
#[derive(Clone)]
pub struct PyRuleFallback;

#[pymethods]
impl PyRuleFallback {
    /// 构造（无参数）
    #[new]
    fn new() -> Self {
        PyRuleFallback
    }
}

/// Python 侧门控配置
#[pyclass(name = "CascadeConfig", skip_from_py_object)]
#[derive(Clone, Copy)]
pub struct PyCascadeConfig {
    pub(crate) inner: CascadeConfig,
}

#[pymethods]
impl PyCascadeConfig {
    /// 构造（默认 0.80 / 0.70）
    #[new]
    #[pyo3(signature = (open_threshold=0.80, hold_threshold=0.70))]
    fn new(open_threshold: f64, hold_threshold: f64) -> Self {
        Self {
            inner: CascadeConfig {
                open_threshold,
                hold_threshold,
            },
        }
    }
}

/// 把 Python 侧 LLMBackend 适配为决策兜底（仅绑定层耦合具体 OpenAICompatBackend）
struct PyLlmFallbackAdapter {
    llm: Arc<TokioMutex<OpenAICompatBackend>>,
}

#[async_trait::async_trait]
impl FallbackDecider for PyLlmFallbackAdapter {
    async fn decide(
        &self,
        state: &serde_json::Value,
    ) -> Result<
        (
            crate::decision::types::Action,
            std::collections::BTreeMap<String, f64>,
            f64,
        ),
        DecisionError,
    > {
        let prompt = crate::decision::LlmFallback::default_template();
        let state_str = state.to_string();
        let vars = std::collections::HashMap::from([("state", state_str.as_str())]);
        let user = prompt.render_user(&vars);
        let messages = vec![
            Message::system(prompt.system_with_few_shot()),
            Message::user(user),
        ];
        let guard = self.llm.lock().await;
        let resp = guard
            .complete(&messages)
            .await
            .map_err(crate::decision::llm_fallback::map_llm_error)?;
        drop(guard);
        let content = resp
            .content
            .ok_or_else(|| DecisionError::Parse("llm empty".into()))?;
        parse_llm_content(&content)
    }
}

/// Python 侧级联决策层（内部 Arc 包裹，供 sync/async 复用）
#[pyclass(name = "SdmCascadeDecisionLayer", skip_from_py_object)]
pub struct PyCascadeLayer {
    shared: Arc<SdmCascadeDecisionLayer>,
}

fn map_decision_err(e: DecisionError) -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(e.to_string())
}

/// 工厂：按 config["mode"] 构造决策后端
///
/// mode = "stub"：无额外字段
/// mode = "replay"：需 "fixtures"（JSONL 路径）
/// mode = "http"：需 base_url / api_key / model，可选 timeout_secs / max_retries
#[pyfunction]
pub fn make_decision_backend(config: &Bound<'_, PyDict>) -> PyResult<PyDecisionBackend> {
    let v: serde_json::Value = pythonize(config.py(), config.as_any())?;
    let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("stub");

    let inner: Arc<dyn DecisionBackend> = match mode {
        "stub" => Arc::new(StubDecisionBackend::new()),
        "replay" => {
            let path = v.get("fixtures").and_then(|x| x.as_str()).ok_or_else(|| {
                pyo3::exceptions::PyValueError::new_err("replay mode requires fixtures")
            })?;
            let backend = ReplayDecisionBackend::from_jsonl(&PathBuf::from(path))
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
            Arc::new(backend)
        }
        "http" => {
            let get = |key: &str| -> PyResult<String> {
                v.get(key)
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        pyo3::exceptions::PyValueError::new_err(format!("http mode requires {key}"))
                    })
            };
            let cfg = SdmHttpConfig {
                base_url: get("base_url")?,
                api_key: get("api_key")?,
                model: get("model")?,
                timeout_secs: v.get("timeout_secs").and_then(|x| x.as_u64()).unwrap_or(30),
                max_retries: v.get("max_retries").and_then(|x| x.as_u64()).unwrap_or(3) as u32,
            };
            Arc::new(SdmHttpBackend::new(cfg).map_err(map_decision_err)?)
        }
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unknown decision backend mode: {other}"
            )));
        }
    };

    // runtime 由全局 _block_on 统一管理，不再随 backend 走
    Ok(PyDecisionBackend { inner })
}

/// 全局共享 runtime：所有 cascade layer 共用。
/// 用 OnceLock 懒初始化，进程内只建一次；返回 Arc 以便多个 backend 克隆共享。
fn shared_runtime() -> &'static Arc<tokio::runtime::Runtime> {
    static RT: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to create global tokio runtime for axon-llm"),
        )
    })
}

/// 兼容嵌套 runtime 上下文的同步块驱动。
///
/// 两种场景：
/// - **内核回测路径**：`BarContextSim::on_event` 在 `self.runtime.block_on(async decide)`
///   （kernel_host.rs:89）里调 Python strategy，当前线程**已经在 R_K 的 runtime 上下文
///   中**。此线程内再 `handle.block_on` 同一个 runtime → tokio 1.x panic "Cannot start
///   a runtime from within a runtime"。
/// - **纯 CLI / 独立调用**：当前线程无 runtime 上下文 → 共享 runtime 直接 block_on。
///
/// 解法：
/// - `Handle::try_current()` Ok → runtime 上下文已存在，用 `futures::executor::block_on`
///   只驱动 future（不持有 runtime），复用当前上下文；
/// - Err → 共享 runtime `block_on`。
fn _block_on<F: std::future::Future>(future: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(_) => futures::executor::block_on(future),
        Err(_) => shared_runtime().block_on(future),
    }
}

/// 从 Python state/questions 两个 dict 构造 DecisionRequest
fn parse_request<'py>(
    py: Python<'py>,
    state: &Bound<'py, PyAny>,
    questions: &Bound<'py, PyAny>,
) -> PyResult<DecisionRequest> {
    let state_v: serde_json::Value = pythonize(py, state)?;
    let questions_v: serde_json::Value = pythonize(py, questions)?;
    let combined = serde_json::json!({"state": state_v, "questions": questions_v});
    serde_json::from_value(combined)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
}

/// CascadeDecision → Python dict
fn decision_to_dict<'py>(
    py: Python<'py>,
    d: &crate::decision::types::CascadeDecision,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("action", d.action.as_str())?;
    dict.set_item(
        "source",
        match d.source {
            DecisionSource::SdmDirect => "sdm_direct",
            DecisionSource::Fallback => "fallback",
        },
    )?;
    dict.set_item("confidence", d.confidence)?;
    let probs = PyDict::new(py);
    for (k, val) in &d.probabilities {
        probs.set_item(k, *val)?;
    }
    dict.set_item("probabilities", probs)?;
    match &d.fallback_reason {
        Some(r) => dict.set_item("fallback_reason", r)?,
        None => dict.set_item("fallback_reason", py.None())?,
    }
    match &d.sdm_usage {
        Some(u) => {
            let ud = PyDict::new(py);
            ud.set_item("input_tokens", u.input_tokens)?;
            ud.set_item("cost_usd", u.cost_usd)?;
            dict.set_item("sdm_usage", ud)?;
        }
        None => dict.set_item("sdm_usage", py.None())?,
    }
    let subs = PyDict::new(py);
    for (k, val) in &d.sub_signals {
        subs.set_item(k, *val)?;
    }
    dict.set_item("sub_signals", subs)?;
    Ok(dict)
}

#[pymethods]
impl PyCascadeLayer {
    /// 用规则兜底构造级联层
    #[new]
    fn new(
        backend: &PyDecisionBackend,
        _fallback: &PyRuleFallback,
        config: &PyCascadeConfig,
    ) -> Self {
        let layer = SdmCascadeDecisionLayer::new(
            backend.inner.clone(),
            Arc::new(RuleFallback::new()) as Arc<dyn FallbackDecider>,
            config.inner,
        );
        Self {
            shared: Arc::new(layer),
        }
    }

    /// 用 LLM 兜底构造级联层
    #[staticmethod]
    #[pyo3(signature = (backend, llm_backend, config))]
    fn with_llm_fallback(
        backend: &PyDecisionBackend,
        llm_backend: &crate::python::backend::PyLLMBackend,
        config: &PyCascadeConfig,
    ) -> Self {
        let adapter = PyLlmFallbackAdapter {
            llm: llm_backend.inner.clone(),
        };
        let layer = SdmCascadeDecisionLayer::new(
            backend.inner.clone(),
            Arc::new(adapter) as Arc<dyn FallbackDecider>,
            config.inner,
        );
        Self {
            shared: Arc::new(layer),
        }
    }

    /// 同步决策：state/questions 为 dict，返回结果 dict
    ///
    /// 兼容嵌套 runtime：若调用发生在另一个 tokio runtime 上下文里
    /// （Rust kernel 回测循环等），用 block_in_place + Handle.block_on 驱动，
    /// 否则直接用共享 runtime block_on。见 `_block_on`。
    fn decide<'py>(
        &self,
        py: Python<'py>,
        state: &Bound<'py, PyAny>,
        questions: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let req = parse_request(py, state, questions)?;
        let d = _block_on(self.shared.decide(&req));
        decision_to_dict(py, &d)
    }

    /// 异步决策：返回 awaitable，不阻塞 Python 事件循环
    fn decide_async<'py>(
        &self,
        py: Python<'py>,
        state: &Bound<'py, PyAny>,
        questions: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req = parse_request(py, state, questions)?;
        let holder = self.shared.clone();
        future_into_py(py, async move {
            let d = holder.decide(&req).await;
            Python::attach(|py| decision_to_dict(py, &d).map(Bound::unbind))
        })
    }
}

/// 注册 `decision` 子模块
pub fn register_decision_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(make_decision_backend, m)?)?;
    m.add_class::<PyDecisionBackend>()?;
    m.add_class::<PyRuleFallback>()?;
    m.add_class::<PyCascadeConfig>()?;
    m.add_class::<PyCascadeLayer>()?;
    Ok(())
}
