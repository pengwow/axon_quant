//! `EnsembleError` → `PyEnsembleError(PyException)` 统一异常转换。

use axon_core::py_exception;

use crate::error::EnsembleError as RustEnsembleError;
use crate::error::EnsembleError::*;

py_exception!(
    axon_quant._native.ensemble,
    EnsembleError,
    RustEnsembleError,
    {
        NoModels => "NoModels",
        WeightMismatch { .. } => "WeightMismatch",
        InvalidWeights { .. } => "InvalidWeights",
        PredictionFailed { .. } => "PredictionFailed",
        MetaModelFailed(_) => "MetaModelFailed",
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;
    use pyo3::prelude::*;

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
