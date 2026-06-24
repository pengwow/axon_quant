//! Python 异常定义

use axon_core::py_exception;

use crate::error::DefiError as RustDefiError;
use crate::error::DefiError::*;

py_exception!(
    axon_quant._native.defi,
    DefiError,
    RustDefiError,
    {
        UnsupportedChain(_) => "UnsupportedChain",
        RpcError(_) => "RpcError",
        TransactionFailed(_) => "TransactionFailed",
        NoRouteFound => "NoRouteFound",
        SlippageTooHigh { .. } => "SlippageTooHigh",
        RiskRejected(_) => "RiskRejected",
        BridgeError(_) => "BridgeError",
        ContractError(_) => "ContractError",
        ConfigError(_) => "ConfigError",
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;
    use pyo3::prelude::*;

    #[test]
    fn test_to_py_err_unsupported_chain() {
        Python::attach(|py| {
            let err = RustDefiError::UnsupportedChain(999);
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[UnsupportedChain]"));
        });
    }

    #[test]
    fn test_to_py_err_config_error() {
        Python::attach(|py| {
            let err = RustDefiError::ConfigError("test".into());
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[ConfigError]"));
        });
    }

    #[test]
    fn test_to_py_err_handles_all_variants() {
        let variants: Vec<RustDefiError> = vec![
            RustDefiError::UnsupportedChain(1),
            RustDefiError::RpcError("test".into()),
            RustDefiError::TransactionFailed("test".into()),
            RustDefiError::NoRouteFound,
            RustDefiError::SlippageTooHigh {
                actual: 5.0,
                max: 1.0,
            },
            RustDefiError::RiskRejected("test".into()),
            RustDefiError::BridgeError("test".into()),
            RustDefiError::ContractError("test".into()),
            RustDefiError::ConfigError("test".into()),
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
