//! 集成管理器 Python 绑定

use pyo3::prelude::*;

use super::traits::PyPolicy;
use super::types::{PyAction, PyModelWeight, PyObservation};
use super::voting::{PyHardVoteStrategy, PySoftVoteStrategy, PyWeightedVoteStrategy};
use crate::manager::EnsembleManager;
use crate::traits::VotingStrategy;

/// 集成管理器
#[pyclass(name = "EnsembleManager", skip_from_py_object)]
pub struct PyEnsembleManager {
    inner: EnsembleManager,
}

#[pymethods]
impl PyEnsembleManager {
    /// 创建集成管理器
    ///
    /// Args:
    ///     strategy: 投票策略（HardVoteStrategy / SoftVoteStrategy / WeightedVoteStrategy）
    #[new]
    fn new(strategy: &Bound<'_, PyAny>) -> PyResult<Self> {
        // 尝试提取不同的策略类型
        let voting_strategy: Box<dyn VotingStrategy> =
            if let Ok(hard) = strategy.extract::<PyHardVoteStrategy>() {
                Box::new(*hard.inner())
            } else if let Ok(soft) = strategy.extract::<PySoftVoteStrategy>() {
                Box::new(*soft.inner())
            } else if let Ok(weighted) = strategy.extract::<PyWeightedVoteStrategy>() {
                Box::new(weighted.inner().clone())
            } else {
                return Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
                    "strategy must be HardVoteStrategy, SoftVoteStrategy, or WeightedVoteStrategy",
                ));
            };

        Ok(Self {
            inner: EnsembleManager::new(voting_strategy),
        })
    }

    /// 注册一个模型
    ///
    /// Args:
    ///     model: Python callable，接受 Observation dict 返回 Action dict
    ///     name: 模型名称
    ///     model_type: 模型类型
    fn register_model(
        &mut self,
        model: Py<PyAny>,
        name: String,
        model_type: super::types::PyModelType,
    ) {
        let policy = Box::new(PyPolicy::new(model, name, model_type.into()));
        self.inner.register_model(policy);
    }

    /// 设置权重
    fn set_weights(&mut self, weights: Vec<f64>) {
        self.inner.set_weights(weights);
    }

    /// 获取权重
    fn get_weights(&self) -> Vec<PyModelWeight> {
        self.inner
            .get_weights()
            .into_iter()
            .map(PyModelWeight::from)
            .collect()
    }

    /// 预测
    ///
    /// Args:
    ///     observation: 观测
    ///     timestamp: 时间戳
    ///
    /// Returns:
    ///     Action
    fn predict(&mut self, observation: &PyObservation, timestamp: u64) -> PyAction {
        PyAction::from_rust(self.inner.predict(observation.inner(), timestamp))
    }

    /// 计算模型多样性
    fn compute_diversity(&self, observations: Vec<PyObservation>) -> f64 {
        let obs: Vec<_> = observations.iter().map(|o| o.inner().clone()).collect();
        self.inner.compute_diversity(&obs)
    }

    /// 历史长度
    fn history_len(&self) -> usize {
        self.inner.history_len()
    }

    /// 模型数量
    fn model_count(&self) -> usize {
        self.inner.model_count()
    }

    /// 投票策略名称
    fn strategy_name(&self) -> &str {
        self.inner.strategy_name()
    }

    fn __repr__(&self) -> String {
        format!(
            "EnsembleManager(models={}, strategy='{}')",
            self.inner.model_count(),
            self.inner.strategy_name()
        )
    }
}

/// 注册管理器到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyEnsembleManager>()?;
    Ok(())
}
