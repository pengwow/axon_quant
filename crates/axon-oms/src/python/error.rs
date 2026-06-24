//! `OmsError` → `PyOmsError(PyException)` 统一异常转换。
//!
//! 使用 `axon_core::py_exception!` 宏生成异常类 + 错误转换 + 注册函数。

use axon_core::py_exception;

use crate::error::OmsError as RustOmsError;
use crate::error::OmsError::*;

py_exception!(
    axon_quant._native.oms,
    OmsError,
    RustOmsError,
    {
        OrderNotFound(_) => "OrderNotFound",
        InvalidTransition { .. } => "InvalidTransition",
        DuplicateIdempotencyKey(_) => "DuplicateIdempotencyKey",
        AlreadyTerminal(_) => "AlreadyTerminal",
        ExchangeRejected(_) => "ExchangeRejected",
        NetworkError(_) => "NetworkError",
        SerializationError(_) => "SerializationError",
        RecoveryFailed(_) => "RecoveryFailed",
        Portfolio(_) => "Portfolio",
    }
);

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;
    use pyo3::prelude::*;

    #[test]
    fn to_py_err_order_not_found_preserves_code() {
        Python::attach(|py| {
            let err = RustOmsError::OrderNotFound("abc-123".into());
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[OrderNotFound]"),
                "expected `[OrderNotFound]` in message, got: {s}"
            );
        });
    }

    #[test]
    fn to_py_err_invalid_transition_preserves_code() {
        Python::attach(|py| {
            let err = RustOmsError::InvalidTransition {
                from: "New".into(),
                to: "Filled".into(),
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[InvalidTransition]"),
                "expected `[InvalidTransition]` in message, got: {s}"
            );
        });
    }

    #[test]
    fn to_py_err_handles_all_variants() {
        let variants: Vec<RustOmsError> = vec![
            RustOmsError::OrderNotFound("o1".into()),
            RustOmsError::InvalidTransition {
                from: "A".into(),
                to: "B".into(),
            },
            RustOmsError::DuplicateIdempotencyKey("k1".into()),
            RustOmsError::AlreadyTerminal("o1".into()),
            RustOmsError::ExchangeRejected("exchange down".into()),
            RustOmsError::NetworkError("timeout".into()),
            RustOmsError::SerializationError("bad json".into()),
            RustOmsError::RecoveryFailed("snap mismatch".into()),
            RustOmsError::Portfolio("insufficient cash".into()),
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
