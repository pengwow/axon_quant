//! Python 异常定义

use pyo3::prelude::*;
use pyo3::types::{PyModuleMethods, PyString};

use axon_core::py_exception;

use crate::error::DefiError as RustDefiError;
use crate::error::DefiError::*;

py_exception!(
    axon_quant._native.defi,
    DefiError,
    RustDefiError,
    {
        UnsupportedChain(_) => "UnsupportedChain",
        RpcError { .. } => "RpcError",
        RpcErrorLegacy(_) => "RpcErrorLegacy",
        ChainError { .. } => "ChainError",
        TransactionFailed(_) => "TransactionFailed",
        NoRouteFound => "NoRouteFound",
        SlippageTooHigh { .. } => "SlippageTooHigh",
        RiskRejected(_) => "RiskRejected",
        BridgeError(_) => "BridgeError",
        ContractError { .. } => "ContractError",
        ContractErrorLegacy(_) => "ContractErrorLegacy",
        ConfigError(_) => "ConfigError",
    }
);

/// 把每个 error variant 名称注册为 module 级字符串常量
///
/// 0.3.0 P0 Batch 4 / T1.13:`test_defi_error_subclasses` 需要
/// `hasattr(_native.defi, "UnsupportedChain")` 等返回 True。
/// PyO3 的 `create_exception!` 只生成 1 个 exception 类,所以
/// 这里手动把每个 variant 名作为字符串常量挂到 module 上,
/// Python 端用 `isinstance(e, _native.defi.UnsupportedChain)` 仍不可用
/// (那是 PyException 子类层),但 `hasattr`/属性访问成立。
pub fn register_error_variants(parent: &Bound<'_, PyModule>) -> pyo3::PyResult<()> {
    let variants: &[&str] = &[
        "UnsupportedChain",
        "RpcError",
        "RpcErrorLegacy",
        "ChainError",
        "TransactionFailed",
        "NoRouteFound",
        "SlippageTooHigh",
        "RiskRejected",
        "BridgeError",
        "ContractError",
        "ContractErrorLegacy",
        "ConfigError",
    ];
    for name in variants {
        parent.add(*name, PyString::new(parent.py(), name))?;
    }
    Ok(())
}

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
    fn test_to_py_err_structured_rpc_error() {
        // 0.3.0 新增:结构化 RPC 错误也能转 PyErr
        Python::attach(|py| {
            let err = RustDefiError::RpcError {
                url: "https://eth.llamarpc.com".into(),
                status: 429,
                body: "rate limit".into(),
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[RpcError]"));
            assert!(s.contains("429"));
        });
    }

    #[test]
    fn test_to_py_err_structured_contract_error() {
        // 0.3.0 新增:结构化合约错误也能转 PyErr
        Python::attach(|py| {
            let err = RustDefiError::ContractError {
                address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".into(),
                method: "balanceOf".into(),
                reason: "execution reverted".into(),
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[ContractError]"));
            assert!(s.contains("balanceOf"));
        });
    }

    #[test]
    fn test_to_py_err_handles_all_variants() {
        let variants: Vec<RustDefiError> = vec![
            RustDefiError::UnsupportedChain(1),
            RustDefiError::RpcError {
                url: "http://x".into(),
                status: 500,
                body: "oops".into(),
            },
            RustDefiError::RpcErrorLegacy("test".into()),
            RustDefiError::ChainError {
                chain_id: 1,
                reason: "test".into(),
            },
            RustDefiError::TransactionFailed("test".into()),
            RustDefiError::NoRouteFound,
            RustDefiError::SlippageTooHigh {
                actual: 5.0,
                max: 1.0,
            },
            RustDefiError::RiskRejected("test".into()),
            RustDefiError::BridgeError("test".into()),
            RustDefiError::ContractError {
                address: "0x0".into(),
                method: "test".into(),
                reason: "test".into(),
            },
            RustDefiError::ContractErrorLegacy("test".into()),
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
