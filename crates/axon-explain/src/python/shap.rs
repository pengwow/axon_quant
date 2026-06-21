//! KernelSHAP Python 绑定

use pyo3::prelude::*;

use super::traits::PyModelPredictor;
use crate::shap::KernelSHAP;

/// KernelSHAP 解释器
#[pyclass(name = "KernelSHAP", skip_from_py_object)]
pub struct PyKernelSHAP {
    inner: KernelSHAP,
}

#[pymethods]
impl PyKernelSHAP {
    /// 创建 KernelSHAP 解释器
    ///
    /// Args:
    ///     model: Python callable，接受 `list[float]` 返回 `list[float]`
    ///     background: 背景数据集，`list[list[float]]`
    ///     n_samples: 采样数（拟合回归的样本数）
    #[new]
    fn new(model: Py<PyAny>, background: Vec<Vec<f64>>, n_samples: usize) -> PyResult<Self> {
        let predictor = Box::new(PyModelPredictor::new(model));
        let inner = KernelSHAP::try_new(predictor, background, n_samples).map_err(PyErr::from)?;
        Ok(Self { inner })
    }

    /// 计算 SHAP 值
    ///
    /// Args:
    ///     observation: 特征向量 `list[float]`
    ///
    /// Returns:
    ///     SHAP 值列表 `list[float]`
    fn compute_shap(&self, observation: Vec<f64>) -> PyResult<Vec<f64>> {
        self.inner
            .try_compute_shap(&observation)
            .map_err(PyErr::from)
    }

    fn __repr__(&self) -> String {
        "KernelSHAP()".to_string()
    }
}

/// 注册 KernelSHAP 到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyKernelSHAP>()?;
    Ok(())
}
