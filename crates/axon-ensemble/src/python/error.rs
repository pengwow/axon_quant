//! `EnsembleError` → `PyEnsembleError(PyException)` 统一异常转换。

use pyo3::exceptions::PyException;
use pyo3::prelude::*;

use crate::error::EnsembleError as RustEnsembleError;

pyo3::create_exception!(
    axon_quant._native.ensemble,
    EnsembleError,
    PyException,
    "axon-ensemble specific error. Inherits Exception. \
     `args[0]` is a stable error code; `args[1]` is a human-readable message."
);

/// 将 Rust 错误转为 Python 异常
pub fn to_py_err(err: RustEnsembleError) -> PyErr {
    let code = match &err {
        RustEnsembleError::NoModels => "NoModels",
        RustEnsembleError::WeightMismatch { .. } => "WeightMismatch",
        RustEnsembleError::InvalidWeights { .. } => "InvalidWeights",
        RustEnsembleError::PredictionFailed { .. } => "PredictionFailed",
        RustEnsembleError::MetaModelFailed(_) => "MetaModelFailed",
    };
    let msg = format!("[{code}] {err}");
    EnsembleError::new_err((code, msg))
}

impl From<RustEnsembleError> for PyErr {
    fn from(err: RustEnsembleError) -> Self {
        to_py_err(err)
    }
}

/// 注册异常类到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    parent.add("EnsembleError", py.get_type::<EnsembleError>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

    #[test]
    fn to_py_err_no_models_preserves_code() {
        Python::attach(|py| {
            let err = RustEnsembleError::NoModels;
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[NoModels]"));
        });
    }

    #[test]
    fn to_py_err_weight_mismatch_preserves_code() {
        Python::attach(|py| {
            let err = RustEnsembleError::WeightMismatch {
                expected: 3,
                actual: 2,
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[WeightMismatch]"));
        });
    }

    #[test]
    fn to_py_err_handles_all_variants() {
        let variants: Vec<RustEnsembleError> = vec![
            RustEnsembleError::NoModels,
            RustEnsembleError::WeightMismatch {
                expected: 1,
                actual: 2,
            },
            RustEnsembleError::InvalidWeights { sum: 0.5 },
            RustEnsembleError::PredictionFailed {
                model_name: "test".into(),
            },
            RustEnsembleError::MetaModelFailed("test".into()),
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
