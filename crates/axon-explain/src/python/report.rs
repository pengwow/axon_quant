//! 报告生成器 Python 绑定

use pyo3::prelude::*;

use super::types::PyDecisionReport;
use super::types::PyExplanation;
use crate::report::ReportGenerator;

/// 报告生成器（静态方法）
#[pyclass(name = "ReportGenerator", skip_from_py_object)]
pub struct PyReportGenerator;

#[pymethods]
impl PyReportGenerator {
    /// 从解释列表生成完整决策报告
    ///
    /// Args:
    ///     report_id: 报告 ID
    ///     explanations: 解释列表
    ///     period_start: 报告开始时间（ISO 8601 字符串）
    ///     period_end: 报告结束时间（ISO 8601 字符串）
    ///
    /// Returns:
    ///     DecisionReport 对象
    #[staticmethod]
    fn generate_decision_report(
        report_id: &str,
        explanations: Vec<PyExplanation>,
        period_start: &str,
        period_end: &str,
    ) -> PyResult<PyDecisionReport> {
        let start = chrono::DateTime::parse_from_rfc3339(period_start)
            .map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                    "Invalid period_start: {}",
                    e
                ))
            })?
            .with_timezone(&chrono::Utc);
        let end = chrono::DateTime::parse_from_rfc3339(period_end)
            .map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                    "Invalid period_end: {}",
                    e
                ))
            })?
            .with_timezone(&chrono::Utc);

        let rust_explanations: Vec<crate::types::Explanation> =
            explanations.iter().map(|e| e.inner().clone()).collect();

        let report =
            ReportGenerator::generate_decision_report(report_id, rust_explanations, start, end);

        Ok(PyDecisionReport::from_rust(report))
    }

    /// 渲染 HTML 报告
    #[staticmethod]
    fn render_html(report: &PyDecisionReport) -> String {
        ReportGenerator::render_html(report.inner())
    }

    /// 渲染 Markdown 报告
    #[staticmethod]
    fn render_markdown(report: &PyDecisionReport) -> String {
        ReportGenerator::render_markdown(report.inner())
    }

    fn __repr__(&self) -> String {
        "ReportGenerator()".to_string()
    }
}

/// 注册 ReportGenerator 到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyReportGenerator>()?;
    Ok(())
}
