//! PyO3 绑定
//!
//! 提供 Python 接口，支持核心合规功能。

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::types::TradeRecord;
use crate::{ComplianceConfig, ComplianceModule};

/// Python 合规模块包装
#[pyclass(name = "ComplianceModule")]
pub struct PyComplianceModule {
    inner: ComplianceModule,
}

#[pymethods]
impl PyComplianceModule {
    /// 创建新的合规模块
    #[new]
    fn new(config_path: &str) -> PyResult<Self> {
        // 从 TOML 文件加载配置
        let config_str = std::fs::read_to_string(config_path)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(e.to_string()))?;
        let config: ComplianceConfig = toml::from_str(&config_str)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;

        // 创建存储目录
        let storage_path = format!("data/compliance/{}", config.account_id);
        std::fs::create_dir_all(&storage_path)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(e.to_string()))?;

        let inner = ComplianceModule::new(config, std::path::Path::new(&storage_path))
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;

        Ok(Self { inner })
    }

    /// 记录交易
    fn record_trade(&mut self, trade: &Bound<'_, PyDict>) -> PyResult<()> {
        // 将 Python dict 转换为 JSON 字符串
        let json_module = trade.py().import("json")?;
        let json_str: String = json_module.call_method1("dumps", (trade,))?.extract()?;

        // 反序列化为 TradeRecord
        let trade: TradeRecord = serde_json::from_str(&json_str)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;

        self.inner
            .record_trade(trade)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;

        Ok(())
    }

    /// 生成日报
    fn generate_daily_report(
        &self,
        py: Python<'_>,
        date: &str,
        starting_balance: f64,
    ) -> PyResult<Py<PyAny>> {
        let date = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;

        let report = self.inner.generate_daily_report(date, starting_balance);

        let json = serde_json::to_string(&report)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
        let dict: Py<PyAny> = py.import("json")?.call_method1("loads", (json,))?.unbind();
        Ok(dict)
    }

    /// 生成月报
    fn generate_monthly_report(
        &self,
        py: Python<'_>,
        year: u32,
        month: u32,
    ) -> PyResult<Py<PyAny>> {
        let report = self
            .inner
            .generate_monthly_report(year, month)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;

        let json = serde_json::to_string(&report)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
        let dict: Py<PyAny> = py.import("json")?.call_method1("loads", (json,))?.unbind();
        Ok(dict)
    }

    /// 生成年报
    fn generate_annual_report(
        &self,
        py: Python<'_>,
        year: u32,
        initial_balance: f64,
    ) -> PyResult<Py<PyAny>> {
        let report = self.inner.generate_annual_report(year, initial_balance);

        let json = serde_json::to_string(&report)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
        let dict: Py<PyAny> = py.import("json")?.call_method1("loads", (json,))?.unbind();
        Ok(dict)
    }

    /// 验证审计完整性
    fn verify_audit_integrity(&self) -> bool {
        self.inner.verify_audit_integrity()
    }

    fn __repr__(&self) -> String {
        format!(
            "ComplianceModule(account_id='{}')",
            self.inner.config().account_id
        )
    }
}

/// Python 模块定义
#[pymodule]
fn axon_compliance(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyComplianceModule>()?;
    Ok(())
}

/// 在父模块（`_native.compliance`）下注册全部已实现的子模块。
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyComplianceModule>()?;
    Ok(())
}
