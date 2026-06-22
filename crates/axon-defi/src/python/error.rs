//! Python 异常定义

use pyo3::exceptions::PyException;
use pyo3::prelude::*;

use crate::error::DefiError as RustDefiError;

pyo3::create_exception!(
    axon_quant._native.defi,
    DefiError,
    PyException,
    "DeFi error"
);

/// 将 Rust 错误转为 Python 异常
pub fn to_py_err(err: RustDefiError) -> PyErr {
    let code = match &err {
        RustDefiError::UnsupportedChain(_) => "UnsupportedChain",
        RustDefiError::RpcError(_) => "RpcError",
        RustDefiError::TransactionFailed(_) => "TransactionFailed",
        RustDefiError::NoRouteFound => "NoRouteFound",
        RustDefiError::SlippageTooHigh { .. } => "SlippageTooHigh",
        RustDefiError::RiskRejected(_) => "RiskRejected",
        RustDefiError::BridgeError(_) => "BridgeError",
        RustDefiError::ContractError(_) => "ContractError",
        RustDefiError::ConfigError(_) => "ConfigError",
    };
    let msg = format!("[{code}] {err}");
    DefiError::new_err((code, msg))
}

impl From<RustDefiError> for PyErr {
    fn from(err: RustDefiError) -> Self {
        to_py_err(err)
    }
}

/// 注册异常到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    parent.add("DefiError", py.get_type::<DefiError>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

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
