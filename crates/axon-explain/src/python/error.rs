//! `ExplainabilityError` → `PyExplainError(PyException)` 统一异常转换。

use pyo3::exceptions::PyException;
use pyo3::prelude::*;

use crate::error::ExplainabilityError as RustExplainError;

pyo3::create_exception!(
    axon_quant._native.explain,
    ExplainError,
    PyException,
    "axon-explain specific error. Inherits Exception. \
     `args[0]` is a stable error code; `args[1]` is a human-readable message."
);

/// 将 Rust 错误转为 Python 异常
pub fn to_py_err(err: RustExplainError) -> PyErr {
    let code = match &err {
        RustExplainError::PythonInterop(_) => "PythonInterop",
        RustExplainError::InvalidDimension(_) => "InvalidDimension",
        RustExplainError::SHAPComputationFailed(_) => "SHAPComputationFailed",
        RustExplainError::AttentionExtractionFailed(_) => "AttentionExtractionFailed",
        RustExplainError::FeatureMismatch { .. } => "FeatureMismatch",
        RustExplainError::ModelNotLoaded(_) => "ModelNotLoaded",
        RustExplainError::ReportGenerationFailed(_) => "ReportGenerationFailed",
        RustExplainError::CounterfactualTimeout => "CounterfactualTimeout",
    };
    let msg = format!("[{code}] {err}");
    ExplainError::new_err((code, msg))
}

impl From<RustExplainError> for PyErr {
    fn from(err: RustExplainError) -> Self {
        to_py_err(err)
    }
}

/// 注册异常类到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    parent.add("ExplainError", py.get_type::<ExplainError>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

    #[test]
    fn to_py_err_shap_computation_preserves_code() {
        Python::attach(|py| {
            let err = RustExplainError::SHAPComputationFailed("test".into());
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[SHAPComputationFailed]"));
        });
    }

    #[test]
    fn to_py_err_feature_mismatch_preserves_code() {
        Python::attach(|py| {
            let err = RustExplainError::FeatureMismatch {
                expected: 10,
                actual: 5,
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[FeatureMismatch]"));
        });
    }

    #[test]
    fn to_py_err_handles_all_variants() {
        let variants: Vec<RustExplainError> = vec![
            RustExplainError::PythonInterop("test".into()),
            RustExplainError::InvalidDimension("test".into()),
            RustExplainError::SHAPComputationFailed("test".into()),
            RustExplainError::AttentionExtractionFailed("test".into()),
            RustExplainError::FeatureMismatch {
                expected: 1,
                actual: 2,
            },
            RustExplainError::ModelNotLoaded("test".into()),
            RustExplainError::ReportGenerationFailed("test".into()),
            RustExplainError::CounterfactualTimeout,
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
