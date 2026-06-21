//! 集成数据类型的 Python 绑定

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::types::{
    Action, ActionProbabilities, ActionType, EnsembleStrategy, ModelType, Observation,
    PortfolioState,
};

/// 模型类型枚举
#[pyclass(name = "ModelType", eq, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyModelType {
    PPO,
    SAC,
    DQN,
    A2C,
    RuleBased,
}

#[pymethods]
impl PyModelType {
    fn __str__(&self) -> &'static str {
        match self {
            Self::PPO => "PPO",
            Self::SAC => "SAC",
            Self::DQN => "DQN",
            Self::A2C => "A2C",
            Self::RuleBased => "RuleBased",
        }
    }

    fn __repr__(&self) -> String {
        format!("ModelType.{}", self.__str__())
    }
}

impl From<ModelType> for PyModelType {
    fn from(t: ModelType) -> Self {
        match t {
            ModelType::PPO => Self::PPO,
            ModelType::SAC => Self::SAC,
            ModelType::DQN => Self::DQN,
            ModelType::A2C => Self::A2C,
            ModelType::RuleBased => Self::RuleBased,
        }
    }
}

impl From<PyModelType> for ModelType {
    fn from(t: PyModelType) -> Self {
        match t {
            PyModelType::PPO => Self::PPO,
            PyModelType::SAC => Self::SAC,
            PyModelType::DQN => Self::DQN,
            PyModelType::A2C => Self::A2C,
            PyModelType::RuleBased => Self::RuleBased,
        }
    }
}

/// 动作类型枚举
#[pyclass(name = "ActionType", eq, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyActionType {
    Buy,
    Sell,
    Hold,
}

#[pymethods]
impl PyActionType {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
            Self::Hold => "hold",
        }
    }

    fn __repr__(&self) -> String {
        format!("ActionType.{}", self.__str__())
    }
}

impl From<ActionType> for PyActionType {
    fn from(t: ActionType) -> Self {
        match t {
            ActionType::Buy => Self::Buy,
            ActionType::Sell => Self::Sell,
            ActionType::Hold => Self::Hold,
        }
    }
}

impl From<PyActionType> for ActionType {
    fn from(t: PyActionType) -> Self {
        match t {
            PyActionType::Buy => Self::Buy,
            PyActionType::Sell => Self::Sell,
            PyActionType::Hold => Self::Hold,
        }
    }
}

/// 集成策略类型枚举
#[pyclass(name = "EnsembleStrategy", eq, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyEnsembleStrategy {
    HardVote,
    SoftVote,
    WeightedVote,
    Stacking,
    DynamicWeighted,
}

#[pymethods]
impl PyEnsembleStrategy {
    fn __str__(&self) -> &'static str {
        match self {
            Self::HardVote => "hard_vote",
            Self::SoftVote => "soft_vote",
            Self::WeightedVote => "weighted_vote",
            Self::Stacking => "stacking",
            Self::DynamicWeighted => "dynamic_weighted",
        }
    }

    fn __repr__(&self) -> String {
        format!("EnsembleStrategy.{}", self.__str__())
    }
}

impl From<EnsembleStrategy> for PyEnsembleStrategy {
    fn from(s: EnsembleStrategy) -> Self {
        match s {
            EnsembleStrategy::HardVote => Self::HardVote,
            EnsembleStrategy::SoftVote => Self::SoftVote,
            EnsembleStrategy::WeightedVote => Self::WeightedVote,
            EnsembleStrategy::Stacking => Self::Stacking,
            EnsembleStrategy::DynamicWeighted => Self::DynamicWeighted,
        }
    }
}

/// 动作概率分布
#[pyclass(name = "ActionProbabilities", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyActionProbabilities {
    inner: ActionProbabilities,
}

#[pymethods]
impl PyActionProbabilities {
    /// 创建动作概率分布（自动归一化）
    #[new]
    fn new(buy: f64, sell: f64, hold: f64) -> Self {
        Self {
            inner: ActionProbabilities::new(buy, sell, hold),
        }
    }

    #[getter]
    fn buy(&self) -> f64 {
        self.inner.buy
    }

    #[getter]
    fn sell(&self) -> f64 {
        self.inner.sell
    }

    #[getter]
    fn hold(&self) -> f64 {
        self.inner.hold
    }

    /// 转为 [buy, sell, hold] 列表
    fn to_list(&self) -> Vec<f64> {
        self.inner.to_vec()
    }

    fn __repr__(&self) -> String {
        format!(
            "ActionProbabilities(buy={:.4}, sell={:.4}, hold={:.4})",
            self.inner.buy, self.inner.sell, self.inner.hold
        )
    }
}

impl PyActionProbabilities {
    /// 从 Rust 类型创建
    pub fn from_rust(p: ActionProbabilities) -> Self {
        Self { inner: p }
    }
}

/// 动作
#[pyclass(name = "Action", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyAction {
    inner: Action,
}

#[pymethods]
impl PyAction {
    #[new]
    fn new(
        action_type: PyActionType,
        symbol: Option<String>,
        quantity: Option<f64>,
        confidence: f64,
    ) -> Self {
        Self {
            inner: Action {
                action_type: action_type.into(),
                symbol,
                quantity,
                confidence,
            },
        }
    }

    #[getter]
    fn action_type(&self) -> PyActionType {
        self.inner.action_type.into()
    }

    #[getter]
    fn symbol(&self) -> Option<&str> {
        self.inner.symbol.as_deref()
    }

    #[getter]
    fn quantity(&self) -> Option<f64> {
        self.inner.quantity
    }

    #[getter]
    fn confidence(&self) -> f64 {
        self.inner.confidence
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        let action_type_str = match self.inner.action_type {
            ActionType::Buy => "buy",
            ActionType::Sell => "sell",
            ActionType::Hold => "hold",
        };
        dict.set_item("action_type", action_type_str)?;
        dict.set_item("symbol", &self.inner.symbol)?;
        dict.set_item("quantity", self.inner.quantity)?;
        dict.set_item("confidence", self.inner.confidence)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        let action_type_str = match self.inner.action_type {
            ActionType::Buy => "buy",
            ActionType::Sell => "sell",
            ActionType::Hold => "hold",
        };
        format!(
            "Action(type='{}', symbol={:?}, qty={:?}, conf={:.4})",
            action_type_str, self.inner.symbol, self.inner.quantity, self.inner.confidence
        )
    }
}

impl PyAction {
    /// 从 Rust 类型创建
    pub fn from_rust(a: Action) -> Self {
        Self { inner: a }
    }

    /// 获取内部引用
    pub fn inner(&self) -> &Action {
        &self.inner
    }
}

/// 观测（模型输入）
#[pyclass(name = "Observation", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyObservation {
    inner: Observation,
}

#[pymethods]
impl PyObservation {
    /// 创建观测
    #[new]
    fn new(
        market_features: Vec<f64>,
        technical_indicators: Vec<f64>,
        time_features: Vec<f64>,
    ) -> Self {
        Self {
            inner: Observation {
                market_features,
                technical_indicators,
                portfolio_state: PortfolioState::default(),
                time_features,
            },
        }
    }

    #[getter]
    fn market_features(&self) -> Vec<f64> {
        self.inner.market_features.clone()
    }

    #[getter]
    fn technical_indicators(&self) -> Vec<f64> {
        self.inner.technical_indicators.clone()
    }

    #[getter]
    fn time_features(&self) -> Vec<f64> {
        self.inner.time_features.clone()
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("market_features", &self.inner.market_features)?;
        dict.set_item("technical_indicators", &self.inner.technical_indicators)?;
        dict.set_item("time_features", &self.inner.time_features)?;
        Ok(dict)
    }
}

impl PyObservation {
    /// 从 Rust 类型创建
    pub fn from_rust(o: Observation) -> Self {
        Self { inner: o }
    }

    /// 获取内部引用
    pub fn inner(&self) -> &Observation {
        &self.inner
    }
}

/// 模型权重
#[pyclass(name = "ModelWeight", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyModelWeight {
    model_name: String,
    weight: f64,
    last_updated: u64,
}

#[pymethods]
impl PyModelWeight {
    #[getter]
    fn model_name(&self) -> &str {
        &self.model_name
    }

    #[getter]
    fn weight(&self) -> f64 {
        self.weight
    }

    #[getter]
    fn last_updated(&self) -> u64 {
        self.last_updated
    }

    fn __repr__(&self) -> String {
        format!(
            "ModelWeight(name='{}', weight={:.4})",
            self.model_name, self.weight
        )
    }
}

impl From<crate::types::ModelWeight> for PyModelWeight {
    fn from(w: crate::types::ModelWeight) -> Self {
        Self {
            model_name: w.model_name,
            weight: w.weight,
            last_updated: w.last_updated,
        }
    }
}

/// 注册类型到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyModelType>()?;
    parent.add_class::<PyActionType>()?;
    parent.add_class::<PyEnsembleStrategy>()?;
    parent.add_class::<PyActionProbabilities>()?;
    parent.add_class::<PyAction>()?;
    parent.add_class::<PyObservation>()?;
    parent.add_class::<PyModelWeight>()?;
    Ok(())
}
