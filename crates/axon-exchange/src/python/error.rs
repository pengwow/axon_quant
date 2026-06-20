//! `ExchangeError` → `PyExchangeError(PyException)` 桥。
//!
//! 设计原因:同 `BacktestError` / `RiskError` / `OmsError`,`ExchangeError`
//! 继承 builtin `PyException` 而非 `AxonError`,避免 `axon-exchange`
//! 反向依赖 `axon-python` 造成 cargo 循环(详见 design spec §3.1.6)。
//!
//! 错误码通过 Debug 输出截取变体名(`Variant` / `Variant { ... }` 的
//! 第一段),稳定用于跨语言错误处理:
//! - Python 端 `e.args[0]` 拿 code(如 `"OrderRejected"`)
//! - Python 端 `e.args[1]` 拿 `[code] details` 形式的展示串

use pyo3::exceptions::PyException;
use pyo3::prelude::*;

use crate::error::ExchangeError as RustExchangeError;

pyo3::create_exception!(
    axon_quant._native.exchange,
    ExchangeError,
    PyException,
    "axon-exchange specific error. Inherits Exception. \
     `args[0]` is a stable error code (e.g. \"OrderRejected\" or \"ApiError\"); \
     `args[1]` is a human-readable message in the form `[<code>] <details>`."
);

/// `RustExchangeError` → `PyErr` 转换。
///
/// 错误码通过 Debug 输出截取变体名,稳定。
pub fn to_py_err(err: RustExchangeError) -> PyErr {
    // `{:?}` 对 enum 输出格式:
    // - 无字段变体:`Variant`(如 `CircuitBreakerOpen`)
    // - tuple variant:`Variant(inner)`(如 `OrderNotFound("xxx")`)
    // - struct variant:`Variant { field: value }`(如 `OrderRejected { reason: "..." }`)
    // 策略:取第一个 `{` / ` ` / `(` 之前的子串作为变体名
    let raw = format!("{err:?}");
    let code = raw
        .split(['{', ' ', '('])
        .next()
        .unwrap_or("Unknown")
        .to_string();
    let msg = format!("[{code}] {err}");
    ExchangeError::new_err((code, msg))
}

impl From<RustExchangeError> for PyErr {
    fn from(err: RustExchangeError) -> Self {
        to_py_err(err)
    }
}

/// 在父模块下注册 `ExchangeError` 异常类。
///
/// 用 `py.get_type::<ExchangeError>()` 拿 PyType,然后 `parent.add`
/// 挂到 `_native.exchange` 子模块上。不依赖 `axon-python` 的 `_native`
/// Rust 模块,避免 cargo 循环。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    parent.add("ExchangeError", py.get_type::<ExchangeError>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Debug 输出截取变体名:`Variant { ... }` / `Variant` 都能拿到 `Variant`。
    #[test]
    fn error_code_extraction_order_rejected() {
        let err = RustExchangeError::OrderRejected {
            reason: "min notional".to_string(),
        };
        let raw = format!("{err:?}");
        let code = raw
            .split(['{', ' ', '('])
            .next()
            .unwrap_or("Unknown")
            .to_string();
        assert_eq!(code, "OrderRejected");
    }

    /// 无字段变体也能正确截取(`CircuitBreakerOpen` 无 `{`)。
    #[test]
    fn error_code_extraction_unit_variant() {
        let err = RustExchangeError::CircuitBreakerOpen;
        let raw = format!("{err:?}");
        let code = raw
            .split(['{', ' ', '('])
            .next()
            .unwrap_or("Unknown")
            .to_string();
        assert_eq!(code, "CircuitBreakerOpen");
    }

    /// tuple variant 也能截取(`OrderNotFound("xxx")` → `OrderNotFound`)。
    #[test]
    fn error_code_extraction_tuple_variant() {
        let err = RustExchangeError::OrderNotFound("o1".into());
        let raw = format!("{err:?}");
        let code = raw
            .split(['{', ' ', '('])
            .next()
            .unwrap_or("Unknown")
            .to_string();
        assert_eq!(code, "OrderNotFound");
    }

    /// 嵌套字段变体(`ApiError { code, message }`)也能拿到变体名。
    #[test]
    fn error_code_extraction_api_error() {
        let err = RustExchangeError::ApiError {
            code: -1003,
            message: "too many requests".to_string(),
        };
        let raw = format!("{err:?}");
        let code = raw
            .split(['{', ' ', '('])
            .next()
            .unwrap_or("Unknown")
            .to_string();
        assert_eq!(code, "ApiError");
    }

    /// 所有 13 个变体都能成功转 `PyErr`(不 panic)。
    #[test]
    fn to_py_err_handles_all_variants() {
        let variants: Vec<RustExchangeError> = vec![
            RustExchangeError::ConnectionFailed("conn refused".into()),
            RustExchangeError::WebSocketDisconnected {
                reason: "timeout".into(),
            },
            RustExchangeError::AuthenticationFailed("bad sig".into()),
            RustExchangeError::OrderRejected {
                reason: "min notional".into(),
            },
            RustExchangeError::InsufficientBalance {
                required: rust_decimal::Decimal::from(100),
                available: rust_decimal::Decimal::from(50),
            },
            RustExchangeError::RateLimited { wait_ms: 1000 },
            RustExchangeError::OrderNotFound("o1".into()),
            RustExchangeError::ParseError("bad json".into()),
            RustExchangeError::ApiError {
                code: -1003,
                message: "too many".into(),
            },
            RustExchangeError::WebSocket("ws closed".into()),
            // Serialization(serde_json::from_str::<i32>("bad").unwrap_err()),
            // 注:`Serialization` / `Network` 变体内部包 reqwest/serde 错误,需
            // 真实网络或 JSON 错误才能构造。Stage 5 集成测试时再覆盖。
            RustExchangeError::CircuitBreakerOpen,
        ];
        for v in variants {
            // 转 PyErr 不得 panic
            let _py: PyErr = v.into();
        }
    }

    /// `register` 函数签名稳定(编译期断言)。
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }

    /// `to_py_err` 反推的 `code` 必须出现在 message 中(`[Code] ...` 形式)。
    #[test]
    fn to_py_err_preserves_code_in_message() {
        let err = RustExchangeError::OrderRejected {
            reason: "min notional".to_string(),
        };
        let py_err: PyErr = err.into();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[OrderRejected]"),
                "expected `[OrderRejected]` in message, got: {s}"
            );
        });
    }
}
