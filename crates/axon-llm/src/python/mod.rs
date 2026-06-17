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
use pyo3::types::{PyDict, PyList};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::backends::{OpenAICompatBackend, OpenAICompatConfig};
use crate::config::LLMConfig;

mod backend;
use backend::{PyLLMBackend, PyMessage};

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

/// 把 Python 对象转 `serde_json::Value`(支持 str/int/float/bool/list/dict/None)
#[allow(clippy::only_used_in_recursion)] // `py` 在递归调用中是必要的 Python token
fn pythonize(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    if obj.is_none() {
        return Ok(serde_json::Value::Null);
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(serde_json::Value::String(s));
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(serde_json::Value::Number(i.into()));
    }
    if let Ok(u) = obj.extract::<u64>()
        && let Some(n) = serde_json::Number::from_u128(u as u128)
    {
        return Ok(serde_json::Value::Number(n));
    }
    if let Ok(f) = obj.extract::<f64>() {
        return serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("NaN/Inf not allowed"));
    }
    if let Ok(b) = obj.extract::<bool>() {
        return Ok(serde_json::Value::Bool(b));
    }
    if let Ok(list) = obj.cast::<PyList>() {
        let mut arr = Vec::with_capacity(list.len());
        for item in list.iter() {
            arr.push(pythonize(py, &item)?);
        }
        return Ok(serde_json::Value::Array(arr));
    }
    if let Ok(d) = obj.cast::<PyDict>() {
        let mut map = serde_json::Map::new();
        for (k, v) in d.iter() {
            let key: String = k.extract()?;
            map.insert(key, pythonize(py, &v)?);
        }
        return Ok(serde_json::Value::Object(map));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "unsupported type: {}",
        obj.get_type().name()?
    )))
}

fn type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
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
    m.add_class::<PyLLMBackend>()?;
    m.add_class::<PyMessage>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `type_name` 覆盖所有 serde_json 变体的中文路径
    #[test]
    fn type_name_covers_all_variants() {
        // 覆盖每个 variant,确保未来加新 variant 时必须显式更新
        assert_eq!(type_name(&serde_json::Value::Null), "null");
        assert_eq!(type_name(&serde_json::Value::Bool(true)), "bool");
        assert_eq!(type_name(&serde_json::json!(1)), "number");
        assert_eq!(type_name(&serde_json::Value::String("x".into())), "string");
        assert_eq!(type_name(&serde_json::json!([])), "array");
        assert_eq!(type_name(&serde_json::json!({})), "object");
    }
}
