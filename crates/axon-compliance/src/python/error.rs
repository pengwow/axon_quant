//! `ComplianceError` → `PyComplianceError(PyException)` 统一异常转换。
//!
//! 设计约束(同 Stage 1-6,详见 design spec §3.1.6):
//! - `ComplianceError` 继承 builtin `PyException` 而非 `AxonError`,
//!   避免 `axon-compliance` 反向依赖 `axon-python` 造成 cargo 循环。
//! - 用 `axon-core::py_exception!` 宏生成 `to_py_err` + `From` + `register`,
//!   避免手写重复代码(同 `axon-explain::python::error` / `axon-oms::python::error`)。
//! - Python 端可走 `except Exception` 统一捕获,或 `except (AxonError, ComplianceError)` 显式列举。

use axon_core::py_exception;

use crate::error::ComplianceError as RustComplianceError;
use crate::error::ComplianceError::*;

py_exception!(
    axon_quant._native.compliance,
    ComplianceError,
    RustComplianceError,
    {
        InvalidTradeData(_) => "InvalidTradeData",
        ConcentrationLimitBreached { .. } => "ConcentrationLimitBreached",
        LargeTradeThresholdExceeded { .. } => "LargeTradeThresholdExceeded",
        AuditIntegrityFailed => "AuditIntegrityFailed",
        StorageError(_) => "StorageError",
        SerializationError(_) => "SerializationError",
        ReportError(_) => "ReportError",
        RegulatorFormatError(_) => "RegulatorFormatError",
        ConfigError(_) => "ConfigError",
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::prelude::*;
    use pyo3::Python;

    #[test]
    fn to_py_err_invalid_trade_preserves_code() {
        Python::attach(|py| {
            let err = RustComplianceError::InvalidTradeData("qty <= 0".into());
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[InvalidTradeData]"), "got: {s}");
        });
    }

    #[test]
    fn to_py_err_concentration_limit_preserves_code() {
        Python::attach(|py| {
            let err = RustComplianceError::ConcentrationLimitBreached {
                symbol: "BTCUSDT".into(),
                current_pct: 45.5,
                limit_pct: 40.0,
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[ConcentrationLimitBreached]"), "got: {s}");
        });
    }

    #[test]
    fn to_py_err_handles_all_variants() {
        let variants: Vec<RustComplianceError> = vec![
            RustComplianceError::InvalidTradeData("x".into()),
            RustComplianceError::ConcentrationLimitBreached {
                symbol: "X".into(),
                current_pct: 1.0,
                limit_pct: 0.5,
            },
            RustComplianceError::LargeTradeThresholdExceeded {
                notional: 1_000_000.0,
                threshold: 500_000.0,
            },
            RustComplianceError::AuditIntegrityFailed,
            RustComplianceError::StorageError("x".into()),
            RustComplianceError::SerializationError("x".into()),
            RustComplianceError::ReportError("x".into()),
            RustComplianceError::RegulatorFormatError("x".into()),
            RustComplianceError::ConfigError("x".into()),
        ];
        for v in variants {
            let _py: PyErr = v.into();
        }
    }

    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, pyo3::types::PyModule>) -> pyo3::PyResult<()> = register;
    }
}
