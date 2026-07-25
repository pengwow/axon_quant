//! PyO3 绑定:将 `OllamaBackend` 暴露给 Python
//!
//! `PyOllamaBackend` 持有内部 backend + tokio runtime,使 Python 端可以同步
//! 调用 `chat()` / `chat_with_tools()`,内部把 async 调用桥到 sync。
//!
//! ## Python 用法
//!
//! ```python
//! from axon_llm import make_ollama_backend, LLMMessage
//!
//! backend = make_ollama_backend({
//!     "backends": [{
//!         "base_url": "http://localhost:11434/v1",
//!         "model": "llama3",
//!     }],
//! })
//! resp = backend.chat([LLMMessage("user", "Hi!")])
//! print(resp["content"])
//! ```

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::backend::{LLMBackend, LLMError};
use crate::backends::OllamaBackend;
use crate::config::LLMConfig;
use crate::types::Message;

#[pyclass(name = "OllamaBackend")]
pub struct PyOllamaBackend {
    pub(crate) inner: Arc<Mutex<OllamaBackend>>,
    pub(crate) runtime: Arc<tokio::runtime::Runtime>,
}

#[pymethods]
impl PyOllamaBackend {
    fn chat<'py>(
        &self,
        py: Python<'py>,
        messages: Vec<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let mut msgs: Vec<Message> = Vec::with_capacity(messages.len());
        for m in &messages {
            msgs.push(parse_py_message(m)?);
        }

        let backend = self.inner.clone();
        let resp = self
            .runtime
            .block_on(async move { backend.lock().await.complete(&msgs).await })
            .map_err(map_err)?;

        let dict = PyDict::new(py);
        dict.set_item("content", resp.content.unwrap_or_default())?;
        dict.set_item("finish_reason", format!("{:?}", resp.finish_reason))?;
        dict.set_item("prompt_tokens", resp.token_usage.prompt_tokens)?;
        dict.set_item("completion_tokens", resp.token_usage.completion_tokens)?;
        dict.set_item("total_tokens", resp.token_usage.total_tokens)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        "OllamaBackend".to_string()
    }
}

fn map_err(e: LLMError) -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(e.to_string())
}

fn parse_py_message(obj: &Bound<'_, PyAny>) -> PyResult<Message> {
    use super::backend::PyMessage;

    if let Ok(pym) = obj.extract::<PyMessage>() {
        return Ok(pym.into());
    }
    if let Ok(d) = obj.cast::<PyDict>() {
        let role: String = d
            .get_item("role")?
            .and_then(|v| v.extract().ok())
            .ok_or_else(|| {
                pyo3::exceptions::PyValueError::new_err("dict message missing 'role'")
            })?;
        let content: String = d
            .get_item("content")?
            .and_then(|v| v.extract().ok())
            .unwrap_or_default();
        let tool_call_id: Option<String> = d
            .get_item("tool_call_id")?
            .and_then(|v| v.extract().ok())
            .flatten();
        let tool_calls: Option<String> = d
            .get_item("tool_calls")?
            .and_then(|v| v.extract().ok())
            .flatten();
        return Ok(Message::from(PyMessage {
            role,
            content,
            tool_call_id,
            tool_calls,
        }));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(
        "each message must be LLMMessage or dict",
    ))
}

#[pyfunction]
pub fn make_ollama_backend(
    py: Python<'_>,
    config: &Bound<'_, PyDict>,
) -> PyResult<PyOllamaBackend> {
    let json_value = super::helpers::pythonize(py, config.as_any())?;

    let map: std::collections::HashMap<String, serde_json::Value> = match json_value {
        serde_json::Value::Object(m) => m.into_iter().collect(),
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "config must be a dict, got {}",
                super::helpers::type_name(&other)
            )));
        }
    };

    let cfg = LLMConfig::from_dict(map)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

    let ollama_config = crate::backends::OllamaConfig::from_llm_config(&cfg, 0)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    let backend = OllamaBackend::new(ollama_config);

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    Ok(PyOllamaBackend {
        inner: Arc::new(Mutex::new(backend)),
        runtime: Arc::new(runtime),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendConfig, ExplainConfig, RetryConfig};

    #[test]
    fn parse_py_message_dict_basic() {
        Python::try_attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("role", "user").unwrap();
            dict.set_item("content", "hello").unwrap();

            let msg = parse_py_message(&dict).unwrap();
            assert_eq!(msg.role, crate::types::Role::User);
            assert_eq!(msg.content, "hello");
        });
    }

    #[test]
    fn make_ollama_backend_from_dict() {
        Python::try_attach(|py| {
            let config = PyDict::new(py);

            let backends = PyList::empty(py);
            let backend_dict = PyDict::new(py);
            backend_dict.set_item("name", "ollama").unwrap();
            backend_dict
                .set_item("base_url", "http://localhost:11434/v1")
                .unwrap();
            backend_dict.set_item("api_key", "").unwrap();
            backend_dict.set_item("model", "llama3").unwrap();
            backend_dict.set_item("max_tokens", 1024).unwrap();
            backend_dict.set_item("temperature", 0.7).unwrap();
            backend_dict.set_item("timeout_secs", 60).unwrap();
            backends.append(backend_dict).unwrap();
            config.set_item("backends", backends).unwrap();

            let retry = PyDict::new(py);
            retry.set_item("max_retries", 3).unwrap();
            retry.set_item("initial_backoff_ms", 100).unwrap();
            retry.set_item("max_backoff_ms", 3000).unwrap();
            config.set_item("retry", retry).unwrap();

            let explain = PyDict::new(py);
            config.set_item("explain", explain).unwrap();

            let backend = make_ollama_backend(py, &config);
            assert!(backend.is_ok());
        });
    }

    use pyo3::types::PyList;
}
