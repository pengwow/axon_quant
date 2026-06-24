//! `ExchangeError` → `PyExchangeError(PyException)` 桥。
//!
//! 使用 `axon_core::py_exception!` 宏生成异常类 + 错误转换 + 注册函数。

use axon_core::py_exception;

use crate::error::ExchangeError as RustExchangeError;
use crate::error::ExchangeError::*;

py_exception!(
    axon_quant._native.exchange,
    ExchangeError,
    RustExchangeError,
    {
        ConnectionFailed(_) => "ConnectionFailed",
        WebSocketDisconnected { .. } => "WebSocketDisconnected",
        AuthenticationFailed(_) => "AuthenticationFailed",
        OrderRejected { .. } => "OrderRejected",
        InsufficientBalance { .. } => "InsufficientBalance",
        RateLimited { .. } => "RateLimited",
        OrderNotFound(_) => "OrderNotFound",
        ParseError(_) => "ParseError",
        ApiError { .. } => "ApiError",
        Network(_) => "Network",
        WebSocket(_) => "WebSocket",
        Serialization(_) => "Serialization",
        CircuitBreakerOpen => "CircuitBreakerOpen",
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;
    use pyo3::prelude::*;

    #[test]
    fn error_code_extraction_order_rejected() {
        let err = RustExchangeError::OrderRejected {
            reason: "min notional".to_string(),
        };
        let py_err: PyErr = err.into();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("[OrderRejected]"));
        });
    }

    #[test]
    fn error_code_extraction_unit_variant() {
        let err = RustExchangeError::CircuitBreakerOpen;
        let py_err: PyErr = err.into();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("[CircuitBreakerOpen]"));
        });
    }

    #[test]
    fn error_code_extraction_tuple_variant() {
        let err = RustExchangeError::OrderNotFound("o1".into());
        let py_err: PyErr = err.into();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("[OrderNotFound]"));
        });
    }

    #[test]
    fn error_code_extraction_api_error() {
        let err = RustExchangeError::ApiError {
            code: -1003,
            message: "too many requests".to_string(),
        };
        let py_err: PyErr = err.into();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("[ApiError]"));
        });
    }

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
            RustExchangeError::CircuitBreakerOpen,
        ];
        for v in variants {
            let _py: PyErr = v.into();
        }
    }

    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
