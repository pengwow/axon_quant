//! Python <-> serde_json 互转的共享 helper
//!
//! 把 `axon-llm` 的 PyO3 模块共用的两个工具函数集中:
//! - `pythonize`:Python 对象 → `serde_json::Value`(用于 LLMConfig / trading args 透传)
//! - `type_name`:`serde_json::Value` 变体名(用于错误消息)

use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::types::PyList;

/// 把 Python 对象转 `serde_json::Value`(支持 str/int/float/bool/list/dict/None)
#[allow(clippy::only_used_in_recursion)] // `py` 在递归调用中是必要的 Python token
pub fn pythonize(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
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

/// `serde_json::Value` 变体名(用于错误消息)
pub fn type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `type_name` 覆盖所有 serde_json 变体的中文路径
    ///
    /// **设计决策**:不直接测 `pythonize`(需要 Python GIL,且 cargo test 默认不嵌入
    /// Python 解释器)。`pythonize` 的功能通过 Python 端 E2E(在
    /// `tests/python/test_trading_python_api.py`)间接覆盖。本模块单元测试只覆盖
    /// 纯 Rust 的 `type_name` 工具。
    #[test]
    fn type_name_covers_all_variants() {
        assert_eq!(type_name(&serde_json::Value::Null), "null");
        assert_eq!(type_name(&serde_json::Value::Bool(true)), "bool");
        assert_eq!(type_name(&serde_json::json!(1)), "number");
        assert_eq!(type_name(&serde_json::Value::String("x".into())), "string");
        assert_eq!(type_name(&serde_json::json!([])), "array");
        assert_eq!(type_name(&serde_json::json!({})), "object");
    }
}
