//! `RiskError` → `PyRiskError(PyException)` 统一异常转换。
//!
//! 使用 `axon_core::py_exception!` 宏生成异常类 + 错误转换 + 注册函数。

use axon_core::py_exception;

use crate::error::RiskError as RustRiskError;
use crate::error::RiskError::*;

py_exception!(
    axon_quant._native.risk,
    RiskError,
    RustRiskError,
    {
        CircuitBreakerActive { .. } => "CircuitBreakerActive",
        OrderRejected { .. } => "OrderRejected",
        ConfigInvalid(_) => "ConfigInvalid",
        Overflow(_) => "Overflow",
    }
);

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RiskReason;
    use pyo3::Python;
    use pyo3::prelude::*;

    #[test]
    fn to_py_err_circuit_breaker_preserves_code() {
        Python::attach(|py| {
            let err = RustRiskError::CircuitBreakerActive {
                until: 1_234_567_890,
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[CircuitBreakerActive]"),
                "expected `[CircuitBreakerActive]` in message, got: {s}"
            );
        });
    }

    #[test]
    fn to_py_err_order_rejected_preserves_code() {
        Python::attach(|py| {
            let err = RustRiskError::OrderRejected {
                reason: RiskReason::OrderTooLarge {
                    max: 1000.0,
                    actual: 2000.0,
                },
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[OrderRejected]"),
                "expected `[OrderRejected]` in message, got: {s}"
            );
        });
    }

    #[test]
    fn to_py_err_handles_all_variants() {
        let variants: Vec<RustRiskError> = vec![
            RustRiskError::CircuitBreakerActive { until: 0 },
            RustRiskError::OrderRejected {
                reason: RiskReason::InsufficientMargin {
                    required: 100.0,
                    available: 50.0,
                },
            },
            RustRiskError::ConfigInvalid("bad".into()),
            RustRiskError::Overflow("f64 overflow".into()),
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
