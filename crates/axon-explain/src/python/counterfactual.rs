//! 反事实生成器 Python 绑定

use pyo3::prelude::*;

use crate::counterfactual::CounterfactualConfig;

/// 反事实生成配置
#[pyclass(name = "CounterfactualConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyCounterfactualConfig {
    inner: CounterfactualConfig,
}

#[pymethods]
impl PyCounterfactualConfig {
    #[new]
    fn new() -> Self {
        Self {
            inner: CounterfactualConfig::new(),
        }
    }

    /// 设置最多修改特征数
    #[staticmethod]
    fn with_max_changes(n: usize) -> Self {
        Self {
            inner: CounterfactualConfig::new().with_max_changes(n),
        }
    }

    /// 设置步长（0.0-1.0）
    #[staticmethod]
    fn with_step_size(s: f64) -> Self {
        Self {
            inner: CounterfactualConfig::new().with_step_size(s),
        }
    }

    /// 设置置信度变化阈值
    #[staticmethod]
    fn with_confidence_threshold(t: f64) -> Self {
        Self {
            inner: CounterfactualConfig::new().with_confidence_threshold(t),
        }
    }

    #[getter]
    fn max_changes(&self) -> usize {
        self.inner.max_changes
    }

    #[getter]
    fn step_size(&self) -> f64 {
        self.inner.step_size
    }

    #[getter]
    fn confidence_threshold(&self) -> f64 {
        self.inner.confidence_threshold
    }

    fn __repr__(&self) -> String {
        format!(
            "CounterfactualConfig(max_changes={}, step_size={:.2}, threshold={:.3})",
            self.inner.max_changes, self.inner.step_size, self.inner.confidence_threshold
        )
    }
}

impl PyCounterfactualConfig {
    /// 获取内部配置引用
    pub fn inner(&self) -> &CounterfactualConfig {
        &self.inner
    }
}

/// 注册配置类到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyCounterfactualConfig>()?;
    Ok(())
}
