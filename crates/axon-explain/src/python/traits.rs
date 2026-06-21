//! Python 适配器：将 Python callable 适配为 Rust `ModelPredictor` trait。

use pyo3::prelude::*;

use crate::traits::ModelPredictor;

/// Python 可调用对象适配 `ModelPredictor` trait
pub struct PyModelPredictor {
    callable: Py<PyAny>,
}

impl PyModelPredictor {
    /// 创建新的适配器
    pub fn new(callable: Py<PyAny>) -> Self {
        Self { callable }
    }
}

impl ModelPredictor for PyModelPredictor {
    fn predict(&self, features: &[f64]) -> Vec<f64> {
        Python::attach(|py| {
            self.callable
                .call1(py, (features,))
                .and_then(|result| result.extract::<Vec<f64>>(py))
                .unwrap_or_else(|e| {
                    tracing::error!("Python predict call failed: {}", e);
                    vec![0.0]
                })
        })
    }
}
