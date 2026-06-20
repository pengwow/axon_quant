//! Python 绑定:`MockSource` 与 `Tick` 类型。
//!
//! 设计:
//! - `PyTick` 直接镜像 Rust `Tick` 字段(`ts_ns` / `price` / `qty` / `side`),
//!   用 `i64` / `f64` / `u8` 表达,避免在 Python 端构造 `Timestamp` / `Price`
//!   / `Quantity` / `Side` 包装类型(降低用户心智负担)。
//! - `PyMockSource.with_tick_series(name, count, nanos_per_step, price_fn)`:
//!   在 Rust 端**循环**调 `price_fn(i)` —— 每次 `Python::attach` 拿 GIL,
//!   调用 `py_fn.call1((i,))`,提取 f64。简单可靠(对 count=10K 仍可接受,
//!   Stage 1 不强求零拷贝)。
//!
//! 数据契约参考:`.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md` §8 Stage 1。

use pyo3::prelude::*;

use crate::sources::MockSource as RustMock;
use crate::traits::DataSource;

use axon_core::market::{Side, Tick};
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity};

// ─── Tick ──────────────────────────────────────────────

/// Python 端 `Tick` 数据类(对应 Rust `axon_core::market::Tick`)。
///
/// 字段全部用原生数值类型,Python 端无需构造 `Timestamp` / `Price` /
/// `Quantity` 包装类型。`side` 编码:`0` = Buy,`1` = Sell(与 Rust
/// `Side` 的 `repr(u8)` 一致)。
///
/// Python 用法:
/// ```python
/// from axon_quant._native.data import Tick
/// t = Tick(ts_ns=1_700_000_000_000_000_000, price=100.5, qty=10.0, side=0)
/// assert t.side == 0  # Buy
/// ```
#[pyclass(name = "Tick", from_py_object)]
#[derive(Debug, Clone, Copy)]
pub struct PyTick {
    /// Rust `Tick`(内部存储)
    pub inner: Tick,
}

#[pymethods]
impl PyTick {
    /// 构造一个 Tick。
    ///
    /// `side`: `0` = Buy, `1` = Sell。其他值会被规整为 `0`(Buy)。
    #[new]
    fn new(ts_ns: i64, price: f64, qty: f64, side: u8) -> Self {
        let s = if side == 0 { Side::Buy } else { Side::Sell };
        Self {
            inner: Tick::new(
                Timestamp::from_nanos(ts_ns),
                Price::from_f64(price),
                Quantity::from_f64(qty),
                s,
            ),
        }
    }

    /// 成交时间(Unix epoch 纳秒数)
    #[getter]
    fn ts_ns(&self) -> i64 {
        self.inner.timestamp.nanos
    }

    /// 成交价
    #[getter]
    fn price(&self) -> f64 {
        self.inner.price.as_f64()
    }

    /// 成交量(可负,Position 用负数表示空头持仓)
    #[getter]
    fn qty(&self) -> f64 {
        self.inner.quantity.as_f64()
    }

    /// 主动成交方向:`0` = Buy, `1` = Sell
    #[getter]
    fn side(&self) -> u8 {
        match self.inner.side {
            Side::Buy => 0,
            Side::Sell => 1,
        }
    }

    /// 成交金额 = price × quantity
    fn turnover(&self) -> f64 {
        self.inner.turnover()
    }

    fn __repr__(&self) -> String {
        format!(
            "Tick(ts_ns={}, price={:.4}, qty={:.4}, side={})",
            self.inner.timestamp.nanos,
            self.inner.price.as_f64(),
            self.inner.quantity.as_f64(),
            match self.inner.side {
                Side::Buy => "Buy",
                Side::Sell => "Sell",
            },
        )
    }
}

// ─── MockSource ──────────────────────────────────────────────

/// Python 端 `MockSource`(对应 Rust `axon_data::sources::MockSource`)。
///
/// 用途:测试桩数据源,提供 3 个静态构造器:
/// - `MockSource.empty()`:空 source(name="mock")
/// - `MockSource.with_rows(name, ticks)`:预置 tick 列表
/// - `MockSource.with_tick_series(name, count, nanos_per_step, price_fn)`:
///   按 `price_fn(i)` 生成 `count` 个 tick,时间按 `nanos_per_step` 均匀递增
///
/// Python 用法:
/// ```python
/// from axon_quant._native.data import MockSource
/// src = MockSource.with_tick_series("btc", 100, 1_000_000, lambda i: 100.0 + i)
/// assert src.name == "btc"
/// ```
#[pyclass(name = "MockSource", from_py_object)]
#[derive(Clone)]
pub struct PyMockSource {
    /// Rust `MockSource`(内部存储,`pub(crate)` 在同 crate 内可见,
    /// 供 `service.rs` 后续通过 `inner.query()` 桥接)
    pub inner: RustMock,
}

#[pymethods]
impl PyMockSource {
    /// 空 MockSource(name="mock",无行)。
    #[staticmethod]
    fn empty() -> Self {
        Self {
            inner: RustMock::empty(),
        }
    }

    /// 预置 tick 行的 MockSource。
    #[staticmethod]
    fn with_rows(name: String, ticks: Vec<PyTick>) -> Self {
        let rust_ticks: Vec<Tick> = ticks.into_iter().map(|t| t.inner).collect();
        Self {
            inner: RustMock::with_rows(name, rust_ticks),
        }
    }

    /// 时间序列生成器:按 `price_fn(i)` 生成 `count` 个 tick。
    ///
    /// 时间从 0 开始,按 `nanos_per_step` 均匀递增;价格由 `price_fn(i)` 计算
    /// (Python callable,签名 `f(int) -> float`);side 固定为 Buy;quantity 固定为 1.0。
    ///
    /// 性能提示:每次 `price_fn(i)` 调用需要 GIL(简单可靠);`count=10K` 量级
    /// 约几十毫秒,Stage 1 不强求零拷贝。
    #[staticmethod]
    #[pyo3(signature = (name, count, nanos_per_step, price_fn))]
    fn with_tick_series(
        py: Python<'_>,
        name: String,
        count: usize,
        nanos_per_step: i64,
        price_fn: Py<PyAny>,
    ) -> PyResult<Self> {
        // 在 Rust 端循环调 Python callable —— 每次取 GIL 调用,
        // 对小规模数据(<= 10K) 简单可靠。
        let mut ticks = Vec::with_capacity(count);
        for i in 0..count {
            let p: f64 = price_fn.call1(py, (i,))?.extract(py)?;
            ticks.push(Tick::new(
                Timestamp::from_nanos(i as i64 * nanos_per_step),
                Price::from_f64(p),
                Quantity::from_f64(1.0),
                Side::Buy,
            ));
        }
        Ok(Self {
            inner: RustMock::with_rows(name, ticks),
        })
    }

    /// 数据源名称。
    #[getter]
    fn name(&self) -> String {
        self.inner.name().to_string()
    }

    /// Mock 中的 tick 行数(供 Python 端 sanity check)。
    #[getter]
    fn len(&self) -> usize {
        // `rows` 是 `pub(crate)` 字段,同 crate 内可访问
        self.inner.rows.len()
    }

    /// 是否为空。
    #[getter]
    fn is_empty(&self) -> bool {
        self.inner.rows.is_empty()
    }

    fn __repr__(&self) -> String {
        format!(
            "MockSource(name={}, len={})",
            self.inner.name(),
            self.inner.rows.len(),
        )
    }
}

/// 在 `_native.data` 子模块下注册 `Tick` + `MockSource` 两个类。
///
/// 调用方:`crates/axon-data/src/python/mod.rs::register_module`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyTick>()?;
    parent.add_class::<PyMockSource>()?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    /// `Tick::new` 各字段在 PyO3 ↔ Rust 间一致。
    #[test]
    fn py_tick_new_field_roundtrip() {
        let t = PyTick::new(1_700_000_000_000_000_000, 100.5, 10.0, 0);
        assert_eq!(t.ts_ns(), 1_700_000_000_000_000_000);
        assert!((t.price() - 100.5).abs() < f64::EPSILON);
        assert!((t.qty() - 10.0).abs() < f64::EPSILON);
        assert_eq!(t.side(), 0);
        assert!((t.turnover() - 1005.0).abs() < 1e-6);
    }

    /// `side != 0` 解析为 Sell (=1)。
    #[test]
    fn py_tick_sell_side() {
        let t = PyTick::new(0, 100.0, 1.0, 1);
        assert_eq!(t.side(), 1);
    }

    /// `__repr__` 含关键字段。
    #[test]
    fn py_tick_repr_contains_fields() {
        let t = PyTick::new(0, 100.0, 1.0, 0);
        let r = t.__repr__();
        assert!(r.contains("Tick"));
        assert!(r.contains("100"));
    }

    /// `MockSource::empty()` 默认名 `"mock"`,行数 0。
    #[test]
    fn py_mock_source_empty() {
        let m = PyMockSource::empty();
        assert_eq!(m.name(), "mock");
        assert_eq!(m.len(), 0);
        assert!(m.is_empty());
    }

    /// `MockSource::with_rows` 保留 tick 行数。
    #[test]
    fn py_mock_source_with_rows() {
        let ticks = vec![
            PyTick::new(0, 100.0, 1.0, 0),
            PyTick::new(1_000, 101.0, 1.0, 1),
        ];
        let m = PyMockSource::with_rows("test".into(), ticks);
        assert_eq!(m.name(), "test");
        assert_eq!(m.len(), 2);
    }

    /// `MockSource::with_tick_series` 调 Python callable 生成行。
    #[test]
    fn py_mock_source_with_tick_series() {
        Python::attach(|py| {
            // price_fn = lambda i: 100.0 + i
            let py_fn = pyo3::Python::eval(py, c"lambda i: 100.0 + i", None, None)
                .unwrap()
                .into_any()
                .unbind();
            let m = PyMockSource::with_tick_series(py, "btc".into(), 5, 1_000_000, py_fn).unwrap();
            assert_eq!(m.name(), "btc");
            assert_eq!(m.len(), 5);
        });
    }

    /// `with_tick_series` 对 Python callable raise 异常时转 `PyErr`。
    #[test]
    fn py_mock_source_with_tick_series_propagates_py_err() {
        Python::attach(|py| {
            // price_fn = lambda i: 1/0 —— 会 raise ZeroDivisionError
            let py_fn = pyo3::Python::eval(py, c"lambda i: 1/0", None, None)
                .unwrap()
                .into_any()
                .unbind();
            let r = PyMockSource::with_tick_series(py, "x".into(), 3, 1, py_fn);
            assert!(r.is_err(), "expected PyErr from ZeroDivisionError");
        });
    }
}
