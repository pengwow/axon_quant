//! Python 端 `RiskMetrics` 独立暴露(Stage 3 Task 5)。
//!
//! # 暴露的符号
//!
//! - `RiskMetrics` — 风险指标的 Python 类(对齐 Rust [`crate::metrics::RiskMetrics`])
//! - `risk_metrics_to_dict` — 把 Rust `RiskMetrics` 转 Python `dict` 的 helper
//!
//! # 设计决策
//!
//! - **`RiskMetrics` 独立成类(非 dict)**:Python 用户可写
//!   `m.total_exposure` 而非 `m["total_exposure"]`,类型安全更好。
//!   `to_dict()` 仍可调用,JSON 序列化更便利。
//!
//! - **dict helper 放在这里而非 `engine.rs`**:避免 `engine.rs` 膨胀,
//!   `engine.rs::metrics` 方法委托本文件的 `to_dict()`。这是 **关注点分离**。
//!
//! - **`from_py_object` 不需要**:`RiskMetrics` 主要从 Rust 端产出
//!   (`engine.metrics(portfolio)`),Python 端通常不会构造它;若需
//!   Python 端构造,可走工厂方法 `RiskMetrics.from_dict(...)`。
//!
//! - **`concentration: HashMap<String, f64>` 暴露为 dict**:
//!   内部 `HashMap` 转 `PyDict`,Python 端 `m.concentration["BTC-USDT"]`
//!   风格访问。

use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::metrics::RiskMetrics as RustMetrics;

// ═══════════════════════════════════════════════════════════════════════════
// PyRiskMetrics
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `RiskMetrics` —— 风险指标聚合(NAV / 杠杆 / 回撤 / VaR / 集中度)。
///
/// 字段语义(与 `crates/axon-risk/src/metrics.rs::RiskMetrics` 一一对应):
/// - `total_exposure`:净资产(NAV)
/// - `leverage`:杠杆倍数(`NAV / base_cash`)
/// - `current_drawdown`:当前回撤比例
/// - `daily_realized_pnl`:日内已实现 PnL
/// - `var_95`:95% VaR
/// - `concentration`:`dict[str, float]`,各标的占组合比例
#[pyclass(name = "RiskMetrics", skip_from_py_object)]
#[derive(Debug, Clone)]
pub struct PyRiskMetrics {
    /// 净资产(NAV)
    total_exposure: f64,
    /// 杠杆倍数
    leverage: f64,
    /// 当前回撤比例
    current_drawdown: f64,
    /// 日内已实现 PnL
    daily_realized_pnl: f64,
    /// 95% VaR
    var_95: f64,
    /// 各标的占组合比例
    concentration: HashMap<String, f64>,
}

impl PyRiskMetrics {
    /// 从 Rust [`RiskMetrics`] 构造
    pub fn from_rust(m: &RustMetrics) -> Self {
        Self {
            total_exposure: m.total_exposure,
            leverage: m.leverage,
            current_drawdown: m.current_drawdown,
            daily_realized_pnl: m.daily_realized_pnl,
            var_95: m.var_95,
            concentration: m.concentration.clone(),
        }
    }
}

#[pymethods]
impl PyRiskMetrics {
    /// 构造空 `RiskMetrics`(全 0 + 空 concentration)
    #[new]
    #[pyo3(signature = (
        total_exposure=0.0,
        leverage=0.0,
        current_drawdown=0.0,
        daily_realized_pnl=0.0,
        var_95=0.0,
        concentration=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        total_exposure: f64,
        leverage: f64,
        current_drawdown: f64,
        daily_realized_pnl: f64,
        var_95: f64,
        concentration: Option<HashMap<String, f64>>,
    ) -> Self {
        Self {
            total_exposure,
            leverage,
            current_drawdown,
            daily_realized_pnl,
            var_95,
            concentration: concentration.unwrap_or_default(),
        }
    }

    /// 净资产(NAV)
    #[getter]
    fn total_exposure(&self) -> f64 {
        self.total_exposure
    }

    /// 杠杆倍数(`NAV / base_cash`)
    #[getter]
    fn leverage(&self) -> f64 {
        self.leverage
    }

    /// 当前回撤比例
    #[getter]
    fn current_drawdown(&self) -> f64 {
        self.current_drawdown
    }

    /// 日内已实现 PnL
    #[getter]
    fn daily_realized_pnl(&self) -> f64 {
        self.daily_realized_pnl
    }

    /// 95% VaR
    #[getter]
    fn var_95(&self) -> f64 {
        self.var_95
    }

    /// 各标的占组合比例(`dict[str, float]`)
    #[getter]
    fn concentration<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (k, v) in &self.concentration {
            d.set_item(k, v)?;
        }
        Ok(d)
    }

    /// dict 视图(JSON 序列化友好)
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("total_exposure", self.total_exposure)?;
        d.set_item("leverage", self.leverage)?;
        d.set_item("current_drawdown", self.current_drawdown)?;
        d.set_item("daily_realized_pnl", self.daily_realized_pnl)?;
        d.set_item("var_95", self.var_95)?;
        let conc = PyDict::new(py);
        for (k, v) in &self.concentration {
            conc.set_item(k, v)?;
        }
        d.set_item("concentration", conc)?;
        Ok(d)
    }

    /// 从 dict 构造(工厂方法,便于 Python 端 `RiskMetrics.from_dict(d)`)
    #[staticmethod]
    fn from_dict(dict: &Bound<'_, PyDict>) -> PyResult<Self> {
        let get_f64 = |k: &str| -> PyResult<f64> {
            dict.get_item(k)?
                .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(format!("missing '{k}'")))?
                .extract::<f64>()
                .map_err(|_| {
                    pyo3::exceptions::PyValueError::new_err(format!("'{k}' has wrong type"))
                })
        };
        let total_exposure = get_f64("total_exposure")?;
        let leverage = get_f64("leverage")?;
        let current_drawdown = get_f64("current_drawdown")?;
        let daily_realized_pnl = get_f64("daily_realized_pnl")?;
        let var_95 = get_f64("var_95")?;
        let concentration: HashMap<String, f64> = match dict.get_item("concentration")? {
            Some(v) => v.extract()?,
            None => HashMap::new(),
        };
        Ok(Self {
            total_exposure,
            leverage,
            current_drawdown,
            daily_realized_pnl,
            var_95,
            concentration,
        })
    }

    fn __repr__(&self) -> String {
        // 注:Rust `format!` 不支持 `:.0%` 格式(Python 风格),手动算百分比。
        format!(
            "RiskMetrics(nav={:.2}, lev={:.2}, dd={:.2}%, daily_pnl={:.2}, var95={:.2}, n_positions={})",
            self.total_exposure,
            self.leverage,
            self.current_drawdown * 100.0,
            self.daily_realized_pnl,
            self.var_95,
            self.concentration.len(),
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// dict helper(供 engine.rs 复用)
// ═══════════════════════════════════════════════════════════════════════════

/// 把 Rust `RiskMetrics` 转 Python `dict`(JSON 序列化友好)
///
/// 注:与 `PyRiskMetrics::to_dict()` 字段命名一致(同 `metrics()` 方法返回
/// 风格),便于 `engine.metrics(portfolio).keys()` 与 `RiskMetrics().to_dict().keys()`
/// 路径等价。
///
/// 字段:
/// - `total_exposure` / `leverage` / `current_drawdown` / `daily_realized_pnl` / `var_95`
/// - `concentration`:`dict[str, float]`
pub fn risk_metrics_to_dict<'py>(py: Python<'py>, m: &RustMetrics) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("total_exposure", m.total_exposure)?;
    d.set_item("leverage", m.leverage)?;
    d.set_item("current_drawdown", m.current_drawdown)?;
    d.set_item("daily_realized_pnl", m.daily_realized_pnl)?;
    d.set_item("var_95", m.var_95)?;
    let conc = PyDict::new(py);
    for (k, v) in &m.concentration {
        conc.set_item(k, v)?;
    }
    d.set_item("concentration", conc)?;
    Ok(d)
}

// ═══════════════════════════════════════════════════════════════════════════
// 注册
// ═══════════════════════════════════════════════════════════════════════════

/// 在 `_native.risk` 下注册 `RiskMetrics`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyRiskMetrics>()
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造器默认参数(全 0 + 空 concentration)
    #[test]
    fn default_constructor_zeros() {
        Python::attach(|py| {
            let conc = PyDict::new(py);
            let m = PyRiskMetrics::new(0.0, 0.0, 0.0, 0.0, 0.0, None);
            assert_eq!(m.total_exposure(), 0.0);
            assert_eq!(m.leverage(), 0.0);
            assert_eq!(m.current_drawdown(), 0.0);
            assert_eq!(m.daily_realized_pnl(), 0.0);
            assert_eq!(m.var_95(), 0.0);
            let c = m.concentration(py).unwrap();
            assert_eq!(c.len(), 0);
            // 显式给空 dict,避免 unused 警告
            let _ = conc;
        });
    }

    /// 自定义参数
    #[test]
    fn custom_params() {
        let m = PyRiskMetrics::new(100_000.0, 2.5, 0.05, 500.0, 1_000.0, None);
        assert_eq!(m.total_exposure(), 100_000.0);
        assert_eq!(m.leverage(), 2.5);
        assert_eq!(m.current_drawdown(), 0.05);
        assert_eq!(m.daily_realized_pnl(), 500.0);
        assert_eq!(m.var_95(), 1_000.0);
    }

    /// `from_rust` 桥接 + getter 一致
    #[test]
    fn from_rust_roundtrip() {
        let mut concentration = HashMap::new();
        concentration.insert("BTC-USDT".to_string(), 0.45);
        concentration.insert("ETH-USDT".to_string(), 0.20);
        let rust_m = RustMetrics {
            total_exposure: 50_000.0,
            leverage: 1.5,
            current_drawdown: 0.10,
            daily_realized_pnl: 1_000.0,
            var_95: 2_000.0,
            concentration,
        };
        let py_m = PyRiskMetrics::from_rust(&rust_m);
        assert_eq!(py_m.total_exposure(), 50_000.0);
        assert_eq!(py_m.leverage(), 1.5);
        assert_eq!(py_m.current_drawdown(), 0.10);
        assert_eq!(py_m.daily_realized_pnl(), 1_000.0);
        assert_eq!(py_m.var_95(), 2_000.0);
    }

    /// `to_dict` 字段完整
    #[test]
    fn to_dict_contains_all_fields() {
        Python::attach(|py| {
            let mut concentration = HashMap::new();
            concentration.insert("BTC-USDT".to_string(), 0.45);
            let m = PyRiskMetrics::new(50_000.0, 1.5, 0.10, 1_000.0, 2_000.0, Some(concentration));
            let d = m.to_dict(py).unwrap();
            assert!(d.get_item("total_exposure").unwrap().is_some());
            assert!(d.get_item("leverage").unwrap().is_some());
            assert!(d.get_item("current_drawdown").unwrap().is_some());
            assert!(d.get_item("daily_realized_pnl").unwrap().is_some());
            assert!(d.get_item("var_95").unwrap().is_some());
            let conc_d = d
                .get_item("concentration")
                .unwrap()
                .expect("concentration key");
            assert!(conc_d.is_instance_of::<PyDict>());
        });
    }

    /// `from_dict` 工厂方法
    #[test]
    fn from_dict_roundtrip() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("total_exposure", 50_000.0_f64).unwrap();
            d.set_item("leverage", 1.5_f64).unwrap();
            d.set_item("current_drawdown", 0.10_f64).unwrap();
            d.set_item("daily_realized_pnl", 1_000.0_f64).unwrap();
            d.set_item("var_95", 2_000.0_f64).unwrap();
            let conc = PyDict::new(py);
            conc.set_item("BTC-USDT", 0.5_f64).unwrap();
            d.set_item("concentration", conc).unwrap();

            let m = PyRiskMetrics::from_dict(&d).unwrap();
            assert_eq!(m.total_exposure(), 50_000.0);
            assert_eq!(m.leverage(), 1.5);
            assert_eq!(m.current_drawdown(), 0.10);
            assert_eq!(m.daily_realized_pnl(), 1_000.0);
            assert_eq!(m.var_95(), 2_000.0);
        });
    }

    /// `from_dict` 缺字段 → PyKeyError
    #[test]
    fn from_dict_missing_field_raises() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("total_exposure", 50_000.0_f64).unwrap();
            // 缺 leverage / current_drawdown / ...
            let err = PyRiskMetrics::from_dict(&d).unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyKeyError>(py));
        });
    }

    /// `risk_metrics_to_dict` 字段命名与 PyRiskMetrics.to_dict 一致
    #[test]
    fn risk_metrics_to_dict_helper() {
        Python::attach(|py| {
            let mut concentration = HashMap::new();
            concentration.insert("BTC-USDT".to_string(), 0.6);
            let rust_m = RustMetrics {
                total_exposure: 100_000.0,
                leverage: 2.0,
                current_drawdown: 0.05,
                daily_realized_pnl: 500.0,
                var_95: 1_500.0,
                concentration,
            };
            let d = risk_metrics_to_dict(py, &rust_m).unwrap();
            // 链式解包:`get_item` 返回 `Result<Option<Bound<PyAny>>, _>`,
            // `.unwrap()` 拆 Result,第二个 `.unwrap()` 拆 Option。
            let total: f64 = d
                .get_item("total_exposure")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(total, 100_000.0);
            let lev: f64 = d.get_item("leverage").unwrap().unwrap().extract().unwrap();
            assert_eq!(lev, 2.0);
            // 注:`get_item` 拿到的是 `Bound<PyAny>`,需先 cast 到 `PyDict` 才能
            // 继续 `get_item`(PyAny 上无 `get_item` 方法)。
            let conc_any = d.get_item("concentration").unwrap().unwrap();
            let conc_dict: &Bound<'_, PyDict> = conc_any.cast().unwrap();
            let btc: f64 = conc_dict
                .get_item("BTC-USDT")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(btc, 0.6);
        });
    }

    /// `__repr__` 包含 `RiskMetrics(` 前缀与关键字段
    #[test]
    fn repr_contains_class_name() {
        let m = PyRiskMetrics::new(100_000.0, 2.0, 0.05, 500.0, 1_500.0, None);
        let s = m.__repr__();
        assert!(s.starts_with("RiskMetrics("), "got: {s}");
        assert!(s.contains("nav=100000"), "got: {s}");
        assert!(s.contains("n_positions=0"), "got: {s}");
    }

    /// `register` 函数签名稳定(编译期断言)
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
