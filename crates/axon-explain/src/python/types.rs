//! 可解释性数据类型的 Python 绑定

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::types::{
    ActionAttribution, ActionSnapshot, ContributionDirection, CounterfactualExplanation,
    DecisionReport, Explanation, FeatureContribution,
};

/// 贡献方向枚举
#[pyclass(name = "ContributionDirection", eq, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyContributionDirection {
    /// 正向贡献
    Positive,
    /// 负向贡献
    Negative,
    /// 中性
    Neutral,
}

#[pymethods]
impl PyContributionDirection {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
            Self::Neutral => "neutral",
        }
    }

    fn __repr__(&self) -> String {
        format!("ContributionDirection.{}", self.__str__())
    }
}

impl From<ContributionDirection> for PyContributionDirection {
    fn from(d: ContributionDirection) -> Self {
        match d {
            ContributionDirection::Positive => Self::Positive,
            ContributionDirection::Negative => Self::Negative,
            ContributionDirection::Neutral => Self::Neutral,
        }
    }
}

impl From<PyContributionDirection> for ContributionDirection {
    fn from(d: PyContributionDirection) -> Self {
        match d {
            PyContributionDirection::Positive => Self::Positive,
            PyContributionDirection::Negative => Self::Negative,
            PyContributionDirection::Neutral => Self::Neutral,
        }
    }
}

/// 单个特征对决策的贡献
#[pyclass(name = "FeatureContribution", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyFeatureContribution {
    inner: FeatureContribution,
}

#[pymethods]
impl PyFeatureContribution {
    #[new]
    fn new(
        feature_name: String,
        shap_value: f64,
        feature_value: f64,
        direction: PyContributionDirection,
    ) -> Self {
        Self {
            inner: FeatureContribution {
                feature_name,
                shap_value,
                feature_value,
                direction: direction.into(),
            },
        }
    }

    #[getter]
    fn feature_name(&self) -> &str {
        &self.inner.feature_name
    }

    #[getter]
    fn shap_value(&self) -> f64 {
        self.inner.shap_value
    }

    #[getter]
    fn feature_value(&self) -> f64 {
        self.inner.feature_value
    }

    #[getter]
    fn direction(&self) -> PyContributionDirection {
        self.inner.direction.into()
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("feature_name", &self.inner.feature_name)?;
        dict.set_item("shap_value", self.inner.shap_value)?;
        dict.set_item("feature_value", self.inner.feature_value)?;
        let dir_str = match self.inner.direction {
            ContributionDirection::Positive => "positive",
            ContributionDirection::Negative => "negative",
            ContributionDirection::Neutral => "neutral",
        };
        dict.set_item("direction", dir_str)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        let dir_str = match self.inner.direction {
            ContributionDirection::Positive => "positive",
            ContributionDirection::Negative => "negative",
            ContributionDirection::Neutral => "neutral",
        };
        format!(
            "FeatureContribution(name='{}', shap={:+.4}, value={:.4}, direction={})",
            self.inner.feature_name, self.inner.shap_value, self.inner.feature_value, dir_str
        )
    }
}

impl PyFeatureContribution {
    /// 从 Rust 类型创建
    pub fn from_rust(c: FeatureContribution) -> Self {
        Self { inner: c }
    }
}

/// 交易动作快照
#[pyclass(name = "ActionSnapshot", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyActionSnapshot {
    pub(crate) inner: ActionSnapshot,
}

#[pymethods]
impl PyActionSnapshot {
    #[new]
    fn new(
        position_size: f64,
        entry_price: f64,
        stop_loss: f64,
        take_profit: f64,
        order_type: String,
    ) -> Self {
        Self {
            inner: ActionSnapshot {
                position_size,
                entry_price,
                stop_loss,
                take_profit,
                order_type,
            },
        }
    }

    #[getter]
    fn position_size(&self) -> f64 {
        self.inner.position_size
    }

    #[getter]
    fn entry_price(&self) -> f64 {
        self.inner.entry_price
    }

    #[getter]
    fn stop_loss(&self) -> f64 {
        self.inner.stop_loss
    }

    #[getter]
    fn take_profit(&self) -> f64 {
        self.inner.take_profit
    }

    #[getter]
    fn order_type(&self) -> &str {
        &self.inner.order_type
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("position_size", self.inner.position_size)?;
        dict.set_item("entry_price", self.inner.entry_price)?;
        dict.set_item("stop_loss", self.inner.stop_loss)?;
        dict.set_item("take_profit", self.inner.take_profit)?;
        dict.set_item("order_type", &self.inner.order_type)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!(
            "ActionSnapshot(pos={:.4}, entry={:.2}, SL={:.2}, TP={:.2}, type='{}')",
            self.inner.position_size,
            self.inner.entry_price,
            self.inner.stop_loss,
            self.inner.take_profit,
            self.inner.order_type
        )
    }
}

impl PyActionSnapshot {
    /// 从 Rust 类型创建
    pub fn from_rust(a: ActionSnapshot) -> Self {
        Self { inner: a }
    }

    /// 获取内部引用
    pub fn inner(&self) -> &ActionSnapshot {
        &self.inner
    }
}

/// 单个动作维度的归因
#[pyclass(name = "ActionAttribution", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyActionAttribution {
    inner: ActionAttribution,
}

#[pymethods]
impl PyActionAttribution {
    #[getter]
    fn dimension(&self) -> &str {
        &self.inner.dimension
    }

    #[getter]
    fn predicted_value(&self) -> f64 {
        self.inner.predicted_value
    }

    #[getter]
    fn base_value(&self) -> f64 {
        self.inner.base_value
    }

    #[getter]
    fn feature_contributions(&self) -> Vec<PyFeatureContribution> {
        self.inner
            .feature_contributions
            .iter()
            .map(|c| PyFeatureContribution::from_rust(c.clone()))
            .collect()
    }

    #[getter]
    fn top_positive(&self) -> Vec<PyFeatureContribution> {
        self.inner
            .top_positive
            .iter()
            .map(|c| PyFeatureContribution::from_rust(c.clone()))
            .collect()
    }

    #[getter]
    fn top_negative(&self) -> Vec<PyFeatureContribution> {
        self.inner
            .top_negative
            .iter()
            .map(|c| PyFeatureContribution::from_rust(c.clone()))
            .collect()
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("dimension", &self.inner.dimension)?;
        dict.set_item("predicted_value", self.inner.predicted_value)?;
        dict.set_item("base_value", self.inner.base_value)?;
        Ok(dict)
    }
}

impl PyActionAttribution {
    /// 从 Rust 类型创建
    pub fn from_rust(a: ActionAttribution) -> Self {
        Self { inner: a }
    }
}

/// 反事实解释
#[pyclass(name = "CounterfactualExplanation", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyCounterfactualExplanation {
    inner: CounterfactualExplanation,
}

#[pymethods]
impl PyCounterfactualExplanation {
    #[getter]
    fn original_action(&self) -> PyActionSnapshot {
        PyActionSnapshot::from_rust(self.inner.original_action.clone())
    }

    #[getter]
    fn modified_action(&self) -> PyActionSnapshot {
        PyActionSnapshot::from_rust(self.inner.modified_action.clone())
    }

    #[getter]
    fn changed_features(&self) -> Vec<String> {
        self.inner.changed_features.clone()
    }

    #[getter]
    fn original_confidence(&self) -> f64 {
        self.inner.original_confidence
    }

    #[getter]
    fn new_confidence(&self) -> f64 {
        self.inner.new_confidence
    }

    #[getter]
    fn narrative(&self) -> &str {
        &self.inner.narrative
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("changed_features", &self.inner.changed_features)?;
        dict.set_item("original_confidence", self.inner.original_confidence)?;
        dict.set_item("new_confidence", self.inner.new_confidence)?;
        dict.set_item("narrative", &self.inner.narrative)?;
        Ok(dict)
    }
}

impl PyCounterfactualExplanation {
    /// 从 Rust 类型创建
    pub fn from_rust(c: CounterfactualExplanation) -> Self {
        Self { inner: c }
    }
}

/// 完整决策解释
#[pyclass(name = "Explanation", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyExplanation {
    pub(crate) inner: Explanation,
}

#[pymethods]
impl PyExplanation {
    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }

    #[getter]
    fn observation_id(&self) -> &str {
        &self.inner.observation_id
    }

    #[getter]
    fn action(&self) -> PyActionSnapshot {
        PyActionSnapshot::from_rust(self.inner.action.clone())
    }

    #[getter]
    fn feature_importance<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (k, v) in &self.inner.feature_importance {
            dict.set_item(k, *v)?;
        }
        Ok(dict)
    }

    #[getter]
    fn action_attributions(&self) -> Vec<PyActionAttribution> {
        self.inner
            .action_attributions
            .iter()
            .map(|a| PyActionAttribution::from_rust(a.clone()))
            .collect()
    }

    #[getter]
    fn counterfactuals(&self) -> Vec<PyCounterfactualExplanation> {
        self.inner
            .counterfactuals
            .iter()
            .map(|c| PyCounterfactualExplanation::from_rust(c.clone()))
            .collect()
    }

    #[getter]
    fn summary(&self) -> &str {
        &self.inner.summary
    }

    #[getter]
    fn confidence(&self) -> f64 {
        self.inner.confidence
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("id", &self.inner.id)?;
        dict.set_item("observation_id", &self.inner.observation_id)?;
        dict.set_item("summary", &self.inner.summary)?;
        dict.set_item("confidence", self.inner.confidence)?;
        Ok(dict)
    }
}

impl PyExplanation {
    /// 从 Rust 类型创建
    pub fn from_rust(e: Explanation) -> Self {
        Self { inner: e }
    }

    /// 获取内部引用
    pub fn inner(&self) -> &Explanation {
        &self.inner
    }
}

/// 决策报告
#[pyclass(name = "DecisionReport", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyDecisionReport {
    pub(crate) inner: DecisionReport,
}

#[pymethods]
impl PyDecisionReport {
    #[getter]
    fn report_id(&self) -> &str {
        &self.inner.report_id
    }

    #[getter]
    fn explanations(&self) -> Vec<PyExplanation> {
        self.inner
            .explanations
            .iter()
            .map(|e| PyExplanation::from_rust(e.clone()))
            .collect()
    }

    #[getter]
    fn html_content(&self) -> Option<&str> {
        self.inner.html_content.as_deref()
    }

    #[getter]
    fn markdown_content(&self) -> Option<&str> {
        self.inner.markdown_content.as_deref()
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("report_id", &self.inner.report_id)?;
        dict.set_item("explanation_count", self.inner.explanations.len())?;
        dict.set_item("has_html", self.inner.html_content.is_some())?;
        dict.set_item("has_markdown", self.inner.markdown_content.is_some())?;
        Ok(dict)
    }
}

impl PyDecisionReport {
    /// 从 Rust 类型创建
    pub fn from_rust(r: DecisionReport) -> Self {
        Self { inner: r }
    }

    /// 获取内部引用
    pub fn inner(&self) -> &DecisionReport {
        &self.inner
    }
}

/// 注册类型到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyContributionDirection>()?;
    parent.add_class::<PyFeatureContribution>()?;
    parent.add_class::<PyActionSnapshot>()?;
    parent.add_class::<PyActionAttribution>()?;
    parent.add_class::<PyCounterfactualExplanation>()?;
    parent.add_class::<PyExplanation>()?;
    parent.add_class::<PyDecisionReport>()?;
    Ok(())
}
