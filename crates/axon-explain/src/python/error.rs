//! `ExplainabilityError` → `PyExplainError(PyException)` 统一异常转换。

use axon_core::py_exception;

use crate::error::ExplainabilityError as RustExplainError;
use crate::error::ExplainabilityError::*;

py_exception!(
    axon_quant._native.explain,
    ExplainError,
    RustExplainError,
    {
        PythonInterop(_) => "PythonInterop",
        InvalidDimension(_) => "InvalidDimension",
        SHAPComputationFailed(_) => "SHAPComputationFailed",
        AttentionExtractionFailed(_) => "AttentionExtractionFailed",
        FeatureMismatch { .. } => "FeatureMismatch",
        ModelNotLoaded(_) => "ModelNotLoaded",
        ReportGenerationFailed(_) => "ReportGenerationFailed",
        CounterfactualTimeout => "CounterfactualTimeout",
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;
    use pyo3::prelude::*;

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
