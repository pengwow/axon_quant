//! Python 端 `RiskConfig` —— 暴露所有可调参数,默认值走 Rust `Default::default()`。

use std::time::Duration;

use pyo3::prelude::*;

use crate::config::RiskConfig as RustConfig;

/// Python 端 `RiskConfig` —— 预交易检查的所有阈值参数。
///
/// 字段语义(与 `crates/axon-risk/src/config.rs::RiskConfig` 一一对应):
/// - `max_position_per_instrument` / `max_total_exposure`:仓位/总敞口上限
/// - `max_order_value`:单笔订单最大名义价值
/// - `max_leverage`:最大杠杆倍数
/// - `max_drawdown`:最大回撤比例(0.0-1.0)
/// - `max_daily_loss`:单日最大亏损(正值)
/// - `max_concentration`:单一标的占组合最大比例(0.0-1.0)
/// - `circuit_breaker_cooldown_secs`:熔断冷却秒数(Rust 内部是 `Duration`,
///   Python 端用秒为单位的 `u64` 暴露,降低跨语言 `Duration` 转换复杂度)
// 注:本类需要从 Python 传入(`DefaultRiskEngine(config)` 构造),
// 加 `from_py_object` 让 pyo3 0.28 自动生成 `FromPyObject`。
#[pyclass(name = "RiskConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyRiskConfig {
    pub inner: RustConfig,
}

#[pymethods]
impl PyRiskConfig {
    // 注:函数标 `pub` 是为了 sibling 模块(engine.rs)的 `#[cfg(test)]` 代码
    // 能直接调用 `PyRiskConfig::new(...)` 构造测试实例。`#[pymethods]` 块内
    // 函数默认 private(只在 Python 端可见),Rust 侧跨模块调用必须显式 `pub`。
    #[new]
    #[pyo3(signature = (
        max_position_per_instrument=100_000.0,
        max_total_exposure=1_000_000.0,
        max_order_value=50_000.0,
        max_leverage=5.0,
        max_drawdown=0.15,
        max_daily_loss=10_000.0,
        max_concentration=0.40,
        circuit_breaker_cooldown_secs=3600,
    ))]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        max_position_per_instrument: f64,
        max_total_exposure: f64,
        max_order_value: f64,
        max_leverage: f64,
        max_drawdown: f64,
        max_daily_loss: f64,
        max_concentration: f64,
        circuit_breaker_cooldown_secs: u64,
    ) -> Self {
        let inner = RustConfig {
            max_position_per_instrument,
            max_total_exposure,
            max_order_value,
            max_leverage,
            max_drawdown,
            max_daily_loss,
            max_concentration,
            circuit_breaker_cooldown: Duration::from_secs(circuit_breaker_cooldown_secs),
            // 0.6.0 新增:跨 leg 约束字段 — 用 `Default` 兜底(默认严格 delta 中性)
            ..Default::default()
        };
        Self { inner }
    }

    #[getter]
    fn max_position_per_instrument(&self) -> f64 {
        self.inner.max_position_per_instrument
    }
    #[getter]
    fn max_total_exposure(&self) -> f64 {
        self.inner.max_total_exposure
    }
    #[getter]
    fn max_order_value(&self) -> f64 {
        self.inner.max_order_value
    }
    #[getter]
    fn max_leverage(&self) -> f64 {
        self.inner.max_leverage
    }
    #[getter]
    fn max_drawdown(&self) -> f64 {
        self.inner.max_drawdown
    }
    #[getter]
    fn max_daily_loss(&self) -> f64 {
        self.inner.max_daily_loss
    }
    #[getter]
    fn max_concentration(&self) -> f64 {
        self.inner.max_concentration
    }
    #[getter]
    fn circuit_breaker_cooldown_secs(&self) -> u64 {
        self.inner.circuit_breaker_cooldown.as_secs()
    }

    fn __repr__(&self) -> String {
        // 注:Rust `format!` 不支持 `:.0%` 格式(Python 风格),手动算百分比。
        format!(
            "RiskConfig(max_pos={:.0}, max_lev={:.1}, max_dd={:.0}%, cb_cooldown={}s)",
            self.inner.max_position_per_instrument,
            self.inner.max_leverage,
            self.inner.max_drawdown * 100.0,
            self.inner.circuit_breaker_cooldown.as_secs(),
        )
    }
}

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyRiskConfig>()
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    /// 自定义参数构造后所有 getter 返回构造时传入的值。
    #[test]
    fn config_construct_and_getters() {
        let c = PyRiskConfig::new(1000.0, 5000.0, 500.0, 2.0, 0.1, 1000.0, 0.3, 60);
        assert_eq!(c.max_position_per_instrument(), 1000.0);
        assert_eq!(c.max_total_exposure(), 5000.0);
        assert_eq!(c.max_order_value(), 500.0);
        assert_eq!(c.max_leverage(), 2.0);
        assert_eq!(c.max_drawdown(), 0.1);
        assert_eq!(c.max_daily_loss(), 1000.0);
        assert_eq!(c.max_concentration(), 0.3);
        assert_eq!(c.circuit_breaker_cooldown_secs(), 60);
    }

    /// 缺省参数走 Rust `Default::default()`(对齐 `RiskConfig::default()`)。
    #[test]
    fn config_default_constructor_matches_rust_default() {
        let py = PyRiskConfig::new(
            100_000.0,
            1_000_000.0,
            50_000.0,
            5.0,
            0.15,
            10_000.0,
            0.40,
            3600,
        );
        let rust = RustConfig::default();
        assert_eq!(
            py.max_position_per_instrument(),
            rust.max_position_per_instrument
        );
        assert_eq!(py.max_leverage(), rust.max_leverage);
        assert_eq!(py.max_drawdown(), rust.max_drawdown);
        assert_eq!(
            py.circuit_breaker_cooldown_secs(),
            rust.circuit_breaker_cooldown.as_secs()
        );
    }

    /// `__repr__` 输出稳定包含 `RiskConfig(` 前缀。
    #[test]
    fn config_repr_format() {
        let c = PyRiskConfig::new(1000.0, 5000.0, 500.0, 2.0, 0.1, 1000.0, 0.3, 60);
        let s = c.__repr__();
        assert!(s.starts_with("RiskConfig("), "got: {s}");
        assert!(s.contains("max_lev=2.0"), "got: {s}");
        assert!(s.contains("cb_cooldown=60s"), "got: {s}");
    }

    /// `register` 函数签名稳定(编译期断言)。
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
