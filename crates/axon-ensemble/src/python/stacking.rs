//! 堆叠集成 Python 绑定

use pyo3::prelude::*;

use super::traits::PyPolicy;
use super::types::{PyAction, PyObservation};
use crate::stacking::{MetaModel, StackingEnsemble};

/// 元模型（线性层 + softmax）
#[pyclass(name = "MetaModel", skip_from_py_object)]
pub struct PyMetaModel {
    inner: MetaModel,
}

#[pymethods]
impl PyMetaModel {
    /// 构造元模型
    ///
    /// Args:
    ///     n_features: 输入特征维度
    ///     n_actions: 输出维度（典型为 3：buy/sell/hold）
    #[new]
    fn new(n_features: usize, n_actions: usize) -> Self {
        Self {
            inner: MetaModel::new(n_features, n_actions),
        }
    }

    /// 加载指定权重和偏置
    #[staticmethod]
    fn with_weights(weights: Vec<Vec<f64>>, bias: Vec<f64>) -> Self {
        Self {
            inner: MetaModel::with_weights(weights, bias),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "MetaModel(n_actions={}, n_features={})",
            self.inner.bias.len(),
            self.inner.weights.first().map(|w| w.len()).unwrap_or(0)
        )
    }
}

impl PyMetaModel {
    /// 获取内部引用
    pub fn inner(&self) -> &MetaModel {
        &self.inner
    }
}

/// 堆叠集成
#[pyclass(name = "StackingEnsemble", skip_from_py_object)]
pub struct PyStackingEnsemble {
    inner: StackingEnsemble,
}

#[pymethods]
impl PyStackingEnsemble {
    /// 构造堆叠集成
    ///
    /// Args:
    ///     base_models: 基模型列表，每个是 (callable, name, model_type) 元组
    ///     meta_model: 元模型
    #[new]
    fn new(
        base_models: Vec<(Py<PyAny>, String, super::types::PyModelType)>,
        meta_model: &PyMetaModel,
    ) -> Self {
        let models: Vec<Box<dyn crate::traits::Policy>> = base_models
            .into_iter()
            .map(|(callable, name, model_type)| {
                Box::new(PyPolicy::new(callable, name, model_type.into()))
                    as Box<dyn crate::traits::Policy>
            })
            .collect();

        Self {
            inner: StackingEnsemble::new(models, meta_model.inner().clone()),
        }
    }

    /// 获取基模型数量
    fn base_model_count(&self) -> usize {
        self.inner.base_model_count()
    }

    /// 预测
    fn predict(&self, observation: &PyObservation) -> PyAction {
        PyAction::from_rust(crate::traits::Ensemble::predict(
            &self.inner,
            observation.inner(),
        ))
    }

    fn __repr__(&self) -> String {
        format!(
            "StackingEnsemble(base_models={})",
            self.inner.base_model_count()
        )
    }
}

/// 注册堆叠集成到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyMetaModel>()?;
    parent.add_class::<PyStackingEnsemble>()?;
    Ok(())
}
