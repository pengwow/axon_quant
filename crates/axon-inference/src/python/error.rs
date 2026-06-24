//! `InferenceError` → `PyInferenceError(PyException)` 桥。
//!
//! 使用 `axon_core::py_exception!` 宏生成异常类 + 错误转换 + 注册函数。

use axon_core::py_exception;

use crate::error::InferenceError as RustInfError;
use crate::error::InferenceError::*;

py_exception!(
    axon_quant._native.inference,
    InferenceError,
    RustInfError,
    {
        ModelNotFound { .. } => "ModelNotFound",
        ModelLoadFailed { .. } => "ModelLoadFailed",
        ModelNotLoaded => "ModelNotLoaded",
        InferenceFailed { .. } => "InferenceFailed",
        DimensionMismatch { .. } => "DimensionMismatch",
        DeviceUnavailable { .. } => "DeviceUnavailable",
        HotReloadFailed { .. } => "HotReloadFailed",
        Onnx(_) => "Onnx",
        Tch(_) => "Tch",
        Candle(_) => "Candle",
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;
    use pyo3::prelude::*;
    use std::path::PathBuf;

    #[test]
    fn error_code_extraction_model_not_found() {
        let err = RustInfError::ModelNotFound {
            path: PathBuf::from("/tmp/m.onnx"),
        };
        let py_err: PyErr = err.into();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("[ModelNotFound]"));
        });
    }

    #[test]
    fn error_code_extraction_tuple_variant() {
        let err = RustInfError::Onnx("model missing".into());
        let py_err: PyErr = err.into();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("[Onnx]"));
        });
    }

    #[test]
    fn error_code_extraction_unit_variant() {
        let err = RustInfError::ModelNotLoaded;
        let py_err: PyErr = err.into();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("[ModelNotLoaded]"));
        });
    }

    #[test]
    fn error_code_extraction_dimension_mismatch() {
        let err = RustInfError::DimensionMismatch {
            expected: 128,
            actual: 64,
        };
        let py_err: PyErr = err.into();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("[DimensionMismatch]"));
        });
    }

    #[test]
    fn to_py_err_handles_all_variants() {
        let variants: Vec<RustInfError> = vec![
            RustInfError::ModelNotFound {
                path: PathBuf::from("/tmp/m.onnx"),
            },
            RustInfError::ModelLoadFailed {
                reason: "bad file".into(),
            },
            RustInfError::ModelNotLoaded,
            RustInfError::InferenceFailed {
                reason: "OOM".into(),
            },
            RustInfError::DimensionMismatch {
                expected: 128,
                actual: 64,
            },
            RustInfError::DeviceUnavailable {
                device: crate::error::Device::Cuda(0),
            },
            RustInfError::HotReloadFailed {
                reason: "watcher died".into(),
            },
            RustInfError::Onnx("runtime error".into()),
            RustInfError::Tch("tensor error".into()),
            RustInfError::Candle("shape error".into()),
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
