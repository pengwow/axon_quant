//! 投票策略 Python 绑定

use pyo3::prelude::*;

use crate::voting::{HardVoteStrategy, SoftVoteStrategy, WeightedVoteStrategy};

/// 硬投票策略（多数表决）
#[pyclass(name = "HardVoteStrategy", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyHardVoteStrategy {
    inner: HardVoteStrategy,
}

#[pymethods]
impl PyHardVoteStrategy {
    #[new]
    fn new() -> Self {
        Self {
            inner: HardVoteStrategy,
        }
    }

    fn __repr__(&self) -> String {
        "HardVoteStrategy()".to_string()
    }
}

impl PyHardVoteStrategy {
    /// 获取内部引用
    pub fn inner(&self) -> &HardVoteStrategy {
        &self.inner
    }
}

/// 软投票策略（概率平均）
#[pyclass(name = "SoftVoteStrategy", from_py_object)]
#[derive(Debug, Clone)]
pub struct PySoftVoteStrategy {
    inner: SoftVoteStrategy,
}

#[pymethods]
impl PySoftVoteStrategy {
    #[new]
    fn new() -> Self {
        Self {
            inner: SoftVoteStrategy,
        }
    }

    fn __repr__(&self) -> String {
        "SoftVoteStrategy()".to_string()
    }
}

impl PySoftVoteStrategy {
    /// 获取内部引用
    pub fn inner(&self) -> &SoftVoteStrategy {
        &self.inner
    }
}

/// 加权投票策略
#[pyclass(name = "WeightedVoteStrategy", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyWeightedVoteStrategy {
    inner: WeightedVoteStrategy,
}

#[pymethods]
impl PyWeightedVoteStrategy {
    /// 创建加权投票策略（权重和必须为 1）
    #[new]
    fn new(weights: Vec<f64>) -> PyResult<Self> {
        let inner = WeightedVoteStrategy::new(weights).map_err(PyErr::from)?;
        Ok(Self { inner })
    }

    /// 用均匀权重构造
    #[staticmethod]
    fn uniform(n: usize) -> Self {
        Self {
            inner: WeightedVoteStrategy::uniform(n),
        }
    }

    fn __repr__(&self) -> String {
        "WeightedVoteStrategy()".to_string()
    }
}

impl PyWeightedVoteStrategy {
    /// 获取内部引用
    pub fn inner(&self) -> &WeightedVoteStrategy {
        &self.inner
    }
}

/// 注册投票策略到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyHardVoteStrategy>()?;
    parent.add_class::<PySoftVoteStrategy>()?;
    parent.add_class::<PyWeightedVoteStrategy>()?;
    Ok(())
}
