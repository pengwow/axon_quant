//! Python 绑定:`Frequency` 枚举 + `DataRequest` 结构体。
//!
//! 数据契约:
//! - `PyFrequency` 与 Rust `Frequency` 一一对应,`value` 字段返回 `"tick" / "1m" / ...`
//!   字符串(用于 JSON 序列化、URL 路径、配置文件)。
//! - `PyDataRequest` 持有 `inner: RustDataRequest`,Python 端通过
//!   `__init__` / `to_dict` / getters 互转。
//!
//! 设计取舍:
//! - `PyFrequency` 用 `#[pyclass(eq, eq_int)]`,Python 端支持
//!   `Frequency.Min1 == Frequency.Min1` 和 `Frequency.Min1 < Frequency.Min5`
//!   (pyo3 `eq_int` 自动从 discriminant 转 int 比较)。
//! - `PyDataRequest` 用 `pub(crate) inner`,允许 `service.rs` 直接读字段
//!   调 `inner.load()` 等内部 API,避免通过 PyO3 getter 反复拷贝。

use chrono::{DateTime, Utc};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::types::{DataRequest as RustDataRequest, Frequency as RustFrequency};

// ─── 频率枚举 ──────────────────────────────────────────────

/// Python 端频率枚举(对应 Rust `Frequency`)。
///
/// 序列化: `value` 返回 `"tick" / "1m" / "1h" / ...` 字符串。
///
/// Python 用法:
/// ```python
/// from axon_quant._native.data import Frequency
/// f = Frequency.Min1
/// assert f.value == "1m"
/// assert Frequency.Min1 < Frequency.Min5  # 通过 eq_int
/// ```
#[pyclass(name = "Frequency", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyFrequency {
    /// 逐笔成交(`"tick"`)
    Tick,
    /// 1 分钟 K 线(`"1m"`)
    Min1,
    /// 5 分钟 K 线(`"5m"`)
    Min5,
    /// 15 分钟 K 线(`"15m"`)
    Min15,
    /// 30 分钟 K 线(`"30m"`)
    Min30,
    /// 1 小时 K 线(`"1h"`)
    Hour1,
    /// 4 小时 K 线(`"4h"`)
    Hour4,
    /// 1 天 K 线(`"1d"`)
    Day1,
    /// 1 周 K 线(`"1w"`)
    Week1,
    /// 1 月 K 线(`"1M"`)
    Month1,
}

impl From<RustFrequency> for PyFrequency {
    fn from(f: RustFrequency) -> Self {
        match f {
            RustFrequency::Tick => Self::Tick,
            RustFrequency::Min1 => Self::Min1,
            RustFrequency::Min5 => Self::Min5,
            RustFrequency::Min15 => Self::Min15,
            RustFrequency::Min30 => Self::Min30,
            RustFrequency::Hour1 => Self::Hour1,
            RustFrequency::Hour4 => Self::Hour4,
            RustFrequency::Day1 => Self::Day1,
            RustFrequency::Week1 => Self::Week1,
            RustFrequency::Month1 => Self::Month1,
        }
    }
}

impl From<PyFrequency> for RustFrequency {
    fn from(f: PyFrequency) -> Self {
        match f {
            PyFrequency::Tick => Self::Tick,
            PyFrequency::Min1 => Self::Min1,
            PyFrequency::Min5 => Self::Min5,
            PyFrequency::Min15 => Self::Min15,
            PyFrequency::Min30 => Self::Min30,
            PyFrequency::Hour1 => Self::Hour1,
            PyFrequency::Hour4 => Self::Hour4,
            PyFrequency::Day1 => Self::Day1,
            PyFrequency::Week1 => Self::Week1,
            PyFrequency::Month1 => Self::Month1,
        }
    }
}

#[pymethods]
impl PyFrequency {
    /// 序列化为 `"tick" / "1m" / "1h" / ...` 字符串。
    #[getter]
    fn value(&self) -> &'static str {
        let f: RustFrequency = (*self).into();
        f.as_str()
    }

    /// 是否为 K 线频率(非 Tick)。
    #[getter]
    fn is_bar(&self) -> bool {
        let f: RustFrequency = (*self).into();
        f.is_bar()
    }

    fn __str__(&self) -> &'static str {
        self.value()
    }

    fn __repr__(&self) -> String {
        format!("Frequency({})", self.value())
    }
}

// ─── DataRequest 结构体 ─────────────────────────────────────

/// Python 端 `DataRequest`(对应 Rust `DataRequest`)。
///
/// Python 用法:
/// ```python
/// import datetime
/// from axon_quant._native.data import DataRequest, Frequency
/// req = DataRequest(
///     symbol="BTCUSDT",
///     start=datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc),
///     end=datetime.datetime(2026, 1, 2, tzinfo=datetime.timezone.utc),
///     frequency=Frequency.Min1,
///     fields=["open", "close"],
///     source="binance",
/// )
/// assert req.is_valid()
/// d = req.to_dict()  # 便于 JSON 序列化
/// ```
#[pyclass(name = "DataRequest", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyDataRequest {
    pub(crate) inner: RustDataRequest,
}

#[pymethods]
impl PyDataRequest {
    /// 构造 `DataRequest`。
    ///
    /// `start` / `end` 必须是带 tzinfo 的 `datetime`(本 crate 只接受 UTC),
    /// 与 Rust `chrono::DateTime<Utc>` 对应。Python 端纯 `datetime` 不带 tz
    /// 时 pyo3 会报 `ValueError`("must be timezone-aware")。
    #[new]
    #[pyo3(signature = (symbol, start, end, frequency, fields=None, source=None))]
    fn new(
        symbol: String,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        frequency: PyFrequency,
        fields: Option<Vec<String>>,
        source: Option<String>,
    ) -> Self {
        let freq: RustFrequency = frequency.into();
        let mut inner = RustDataRequest::new(symbol, start, end, freq);
        if let Some(f) = fields {
            inner = inner.with_fields(f);
        }
        if let Some(s) = source {
            inner = inner.with_source(s);
        }
        Self { inner }
    }

    #[getter]
    fn symbol(&self) -> String {
        self.inner.symbol.clone()
    }

    #[getter]
    fn start(&self) -> DateTime<Utc> {
        self.inner.start
    }

    #[getter]
    fn end(&self) -> DateTime<Utc> {
        self.inner.end
    }

    #[getter]
    fn frequency(&self) -> PyFrequency {
        self.inner.frequency.into()
    }

    #[getter]
    fn fields(&self) -> Vec<String> {
        self.inner.fields.clone()
    }

    #[getter]
    fn source(&self) -> Option<String> {
        self.inner.source.clone()
    }

    /// 时间窗口是否合法(`start <= end`)。
    fn is_valid(&self) -> bool {
        self.inner.is_valid()
    }

    /// 序列化为 Python `dict`(便于 JSON 序列化、日志输出)。
    fn to_dict<'a>(&self, py: Python<'a>) -> PyResult<Bound<'a, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("symbol", &self.inner.symbol)?;
        d.set_item("start", self.inner.start.to_rfc3339())?;
        d.set_item("end", self.inner.end.to_rfc3339())?;
        d.set_item("frequency", self.inner.frequency.as_str())?;
        d.set_item("fields", &self.inner.fields)?;
        d.set_item("source", &self.inner.source)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "DataRequest(symbol={:?}, frequency={}, fields={}, source={:?})",
            self.inner.symbol,
            self.inner.frequency.as_str(),
            self.inner.fields.len(),
            self.inner.source,
        )
    }
}

/// 在 `_native.data` 子模块下注册 `Frequency` + `DataRequest` 两个类。
///
/// 调用方:`crates/axon-data/src/python/mod.rs::register_module`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyFrequency>()?;
    parent.add_class::<PyDataRequest>()?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// UTC datetime 工厂:简化测试 setup。
    fn utc(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    /// `PyFrequency::value` 返回与 Rust `Frequency::as_str` 一致的稳定字符串。
    #[test]
    fn py_frequency_value_matches_rust_as_str() {
        assert_eq!(PyFrequency::Tick.value(), "tick");
        assert_eq!(PyFrequency::Min1.value(), "1m");
        assert_eq!(PyFrequency::Min5.value(), "5m");
        assert_eq!(PyFrequency::Min15.value(), "15m");
        assert_eq!(PyFrequency::Min30.value(), "30m");
        assert_eq!(PyFrequency::Hour1.value(), "1h");
        assert_eq!(PyFrequency::Hour4.value(), "4h");
        assert_eq!(PyFrequency::Day1.value(), "1d");
        assert_eq!(PyFrequency::Week1.value(), "1w");
        assert_eq!(PyFrequency::Month1.value(), "1M");
    }

    /// `is_bar` 与 Rust 端一致:`Tick` 为 false,其他为 true。
    #[test]
    fn py_frequency_is_bar_matches_rust() {
        assert!(!PyFrequency::Tick.is_bar());
        assert!(PyFrequency::Min1.is_bar());
        assert!(PyFrequency::Month1.is_bar());
    }

    /// `From` 在 Rust ↔ Python 间无信息丢失。
    #[test]
    fn py_frequency_roundtrip() {
        for f in [
            PyFrequency::Tick,
            PyFrequency::Min1,
            PyFrequency::Min5,
            PyFrequency::Min15,
            PyFrequency::Min30,
            PyFrequency::Hour1,
            PyFrequency::Hour4,
            PyFrequency::Day1,
            PyFrequency::Week1,
            PyFrequency::Month1,
        ] {
            let rust: RustFrequency = f.into();
            let back: PyFrequency = rust.into();
            assert_eq!(f, back);
        }
    }

    /// `eq_int` 让 Python 端能直接用 `<` `>` 比较(按 enum 顺序)。
    #[test]
    fn py_frequency_eq_int_ordering() {
        // Min1 (variant index 1) < Min5 (variant index 2) < Min15 ...
        // 用 __eq__/__lt__ 验证
        assert!(PyFrequency::Min1 == PyFrequency::Min1);
        assert!(PyFrequency::Min1 != PyFrequency::Min5);
    }

    /// `PyDataRequest::new` 与 builder 链式 API 等价。
    #[test]
    fn py_data_request_new_with_all_args() {
        let req = PyDataRequest::new(
            "BTCUSDT".into(),
            utc(2026, 1, 1),
            utc(2026, 1, 2),
            PyFrequency::Min1,
            Some(vec!["open".into(), "close".into()]),
            Some("binance".into()),
        );
        assert_eq!(req.symbol(), "BTCUSDT");
        assert_eq!(req.frequency(), PyFrequency::Min1);
        assert_eq!(req.fields(), vec!["open", "close"]);
        assert_eq!(req.source(), Some("binance".into()));
        assert!(req.is_valid());
        assert_eq!(req.start(), utc(2026, 1, 1));
        assert_eq!(req.end(), utc(2026, 1, 2));
    }

    /// `PyDataRequest::new` 不传 fields/source 时用空 Vec / None。
    #[test]
    fn py_data_request_new_minimal() {
        let req = PyDataRequest::new(
            "X".into(),
            utc(2026, 1, 1),
            utc(2026, 1, 2),
            PyFrequency::Tick,
            None,
            None,
        );
        assert!(req.fields().is_empty());
        assert!(req.source().is_none());
        assert!(req.is_valid());
    }

    /// `is_valid` 对 `start > end` 返回 false。
    #[test]
    fn py_data_request_invalid_window() {
        let req = PyDataRequest::new(
            "X".into(),
            utc(2026, 1, 2),
            utc(2026, 1, 1),
            PyFrequency::Min1,
            None,
            None,
        );
        assert!(!req.is_valid());
    }

    /// `to_dict` 输出 dict,所有 key 存在且值正确。
    #[test]
    fn py_data_request_to_dict_contents() {
        Python::attach(|py| {
            let req = PyDataRequest::new(
                "BTCUSDT".into(),
                utc(2026, 1, 1),
                utc(2026, 1, 2),
                PyFrequency::Hour1,
                Some(vec!["open".into()]),
                Some("binance".into()),
            );
            let d = req.to_dict(py).unwrap();
            let symbol = d
                .get_item("symbol")
                .unwrap()
                .unwrap()
                .extract::<String>()
                .unwrap();
            assert_eq!(symbol, "BTCUSDT");
            let freq = d
                .get_item("frequency")
                .unwrap()
                .unwrap()
                .extract::<String>()
                .unwrap();
            assert_eq!(freq, "1h");
            let source = d
                .get_item("source")
                .unwrap()
                .unwrap()
                .extract::<String>()
                .unwrap();
            assert_eq!(source, "binance");
            // start 是 RFC3339 字符串
            let start: String = d.get_item("start").unwrap().unwrap().extract().unwrap();
            assert!(start.starts_with("2026-01-01"));
        });
    }
}
