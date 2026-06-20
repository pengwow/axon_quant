//! Python 绑定:`Dataset` + `SchemaField` + `DataType`。
//!
//! 数据契约:
//! - `PySchemaField` 镜像 Rust `SchemaField`,提供 `name` + `dtype`(字符串形式)
//! - `PyDataType` 镜像 Rust `DataType`(简化版 5 种类型)
//! - `PyDataset` 持 `Arc<RustDataset>` 共享所有权(零拷贝),
//!   暴露 `to_arrow(i)` 返回 `pyarrow.RecordBatch`(零拷贝),
//!   以及 Rust 端 `take / skip / last_n / by_time_range` 链式切片操作。
//!
//! 设计取舍:
//! - `PyDataset.inner: Arc<RustDataset>`,允许在 `take / skip` 等
//!   产生新 `Dataset` 的方法中,直接 clone Arc(避免复制 Vec<RecordBatch>);
//! - 暴露 `to_arrow_table()` 便利方法,把 `batches` 拼成 `pyarrow.Table` 一次返回;
//! - 暴露 `iter_ticks()` 给 Python 端流式消费(简单可靠,不强求零拷贝)。
//!
//! 数据契约参考:`.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md` §8 Stage 1。

use std::sync::Arc;

use arrow::record_batch::RecordBatch;
use pyo3::prelude::*;
use pyo3_arrow::PyRecordBatch;

use crate::dataset::Dataset as RustDataset;
use crate::types::{
    DataRequest as RustDataRequest, DataType as RustDataType, SchemaField as RustSchemaField,
};

use super::sources::PyTick;
use super::types::PyDataRequest;

// ─── DataType 枚举 ─────────────────────────────────────────

/// Python 端数据类型(对应 Rust 简化版 `DataType`)。
///
/// 与 `PyFrequency` 类似,用 `eq_int` 让 Python 端支持
/// `DataType.F64 == DataType.F64`。
#[pyclass(name = "DataType", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyDataType {
    /// 64 位浮点(`"f64"`)
    F64,
    /// 64 位整数(`"i64"`)
    I64,
    /// 布尔(`"bool"`)
    Bool,
    /// 字符串(`"string"`)
    String,
    /// 时间戳(纳秒,`"timestamp"`)
    Timestamp,
}

impl From<RustDataType> for PyDataType {
    fn from(d: RustDataType) -> Self {
        match d {
            RustDataType::F64 => Self::F64,
            RustDataType::I64 => Self::I64,
            RustDataType::Bool => Self::Bool,
            RustDataType::String => Self::String,
            RustDataType::Timestamp => Self::Timestamp,
        }
    }
}

impl From<PyDataType> for RustDataType {
    fn from(d: PyDataType) -> Self {
        match d {
            PyDataType::F64 => Self::F64,
            PyDataType::I64 => Self::I64,
            PyDataType::Bool => Self::Bool,
            PyDataType::String => Self::String,
            PyDataType::Timestamp => Self::Timestamp,
        }
    }
}

#[pymethods]
impl PyDataType {
    /// 序列化为稳定字符串(用于 JSON / 配置 / 跨语言协议)。
    #[getter]
    fn value(&self) -> &'static str {
        let d: RustDataType = (*self).into();
        match d {
            RustDataType::F64 => "f64",
            RustDataType::I64 => "i64",
            RustDataType::Bool => "bool",
            RustDataType::String => "string",
            RustDataType::Timestamp => "timestamp",
        }
    }

    fn __str__(&self) -> &'static str {
        self.value()
    }

    fn __repr__(&self) -> String {
        format!("DataType({})", self.value())
    }
}

// ─── SchemaField 结构体 ───────────────────────────────────

/// Python 端 `SchemaField`(对应 Rust `SchemaField`)。
///
/// 简单数据类:`name` (str) + `dtype` (DataType 枚举)。
#[pyclass(name = "SchemaField", from_py_object)]
#[derive(Debug, Clone)]
pub struct PySchemaField {
    /// Rust `SchemaField`(内部存储)
    pub inner: RustSchemaField,
}

#[pymethods]
impl PySchemaField {
    /// 构造一个 `SchemaField`。
    ///
    /// `dtype` 是稳定字符串(`"f64"` / `"i64"` / `"bool"` / `"string"` /
    /// `"timestamp"`),与 `DataType.value` 一致。
    #[new]
    fn new(name: String, dtype: &str) -> PyResult<Self> {
        let dt = match dtype {
            "f64" => RustDataType::F64,
            "i64" => RustDataType::I64,
            "bool" => RustDataType::Bool,
            "string" => RustDataType::String,
            "timestamp" => RustDataType::Timestamp,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown dtype: {other} (expected one of: f64/i64/bool/string/timestamp)"
                )));
            }
        };
        Ok(Self {
            inner: RustSchemaField::new(name, dt),
        })
    }

    /// 字段名。
    #[getter]
    fn name(&self) -> String {
        self.inner.name.clone()
    }

    /// 字段类型(`DataType` 枚举实例)。
    #[getter]
    fn dtype(&self) -> PyDataType {
        self.inner.dtype.into()
    }

    fn __repr__(&self) -> String {
        format!(
            "SchemaField(name={:?}, dtype={})",
            self.inner.name,
            self.inner.dtype.value_as_str()
        )
    }
}

// `DataType::value_as_str` 不是 trait/impl 已有方法,需要在本文件补一个
// 内联辅助 trait,保持 `__repr__` 输出稳定字符串。
trait DataTypeStrExt {
    fn value_as_str(&self) -> &'static str;
}

impl DataTypeStrExt for RustDataType {
    fn value_as_str(&self) -> &'static str {
        match self {
            RustDataType::F64 => "f64",
            RustDataType::I64 => "i64",
            RustDataType::Bool => "bool",
            RustDataType::String => "string",
            RustDataType::Timestamp => "timestamp",
        }
    }
}

// ─── Dataset ──────────────────────────────────────────────

/// Python 端 `Dataset`(对应 Rust `axon_data::Dataset`)。
///
/// `Arc<RustDataset>` 共享所有权——`take / skip / last_n / filter` 等
/// 切片操作仅 clone Arc(指针级别),不需要复制 Arrow batches。
///
/// Python 用法:
/// ```python
/// from axon_quant.data import DataService, MockSource, Frequency, DataRequest
/// svc = DataService.new().register_source(MockSource.with_tick_series("m", 100, 1_000_000, lambda i: 100.0 + i))
/// ds = svc.load(DataRequest("X", start_dt, end_dt, Frequency.Tick))
/// assert ds.len == 100
/// # 零拷贝取单个 batch
/// batch = ds.to_arrow(0)
/// assert isinstance(batch, pa.RecordBatch)
/// # 或者一次拿整个 table
/// table = ds.to_arrow_table()
/// ```
#[pyclass(name = "Dataset", from_py_object)]
#[derive(Clone)]
pub struct PyDataset {
    /// Rust `Dataset`(`Arc` 包装,允许切片操作零拷贝共享)
    pub(crate) inner: Arc<RustDataset>,
}

#[pymethods]
impl PyDataset {
    /// 总行数(所有 `batches.num_rows()` 求和)。
    #[getter]
    fn len(&self) -> usize {
        self.inner.len()
    }

    /// 是否为空。
    #[getter]
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// `__len__` 等价于 `len`,支持 Python 内置 `len(ds)`。
    fn __len__(&self) -> usize {
        self.inner.len()
    }

    /// 数据集 ID(UUID v4 字符串)。
    #[getter]
    fn id(&self) -> String {
        self.inner.id.to_string()
    }

    /// 数据源名称(创建时指定)。
    #[getter]
    fn source(&self) -> String {
        self.inner.source.clone()
    }

    /// 加载时间(UTC ISO 8601 字符串)。
    #[getter]
    fn loaded_at(&self) -> String {
        self.inner.loaded_at.to_rfc3339()
    }

    /// SHA256 校验和(64 字符 hex)。
    #[getter]
    fn checksum(&self) -> String {
        self.inner.checksum.clone()
    }

    /// 关联请求(可追溯)。
    #[getter]
    fn request(&self) -> PyDataRequest {
        // DataRequest 是 Clone,从 Rust 端 clone 一份构造 PyDataRequest
        let inner: RustDataRequest = self.inner.request.clone();
        PyDataRequest { inner }
    }

    /// 数据 batch 数。
    #[getter]
    fn num_batches(&self) -> usize {
        self.inner.batches.len()
    }

    /// Schema 字段列表(`[SchemaField(name="timestamp", dtype=DataType("i64")), ...]`)。
    ///
    /// 注:当前 Dataset 的 schema 固定为 4 列(`dataset_schema()` 共享),
    /// 字段名 + 类型来自 Arrow schema,而非 Rust `SchemaField`,
    /// 但用 `DataType` 简化枚举表达,保持对外协议一致。
    fn schema(&self) -> Vec<PySchemaField> {
        self.inner
            .schema
            .fields()
            .iter()
            .map(|f| {
                // 把 Arrow DataType 映射回 Rust 简化版 DataType
                use arrow::datatypes::DataType as ArrowDt;
                let dt = match f.data_type() {
                    ArrowDt::Int64 => RustDataType::I64,
                    ArrowDt::Float64 => RustDataType::F64,
                    ArrowDt::Utf8 => RustDataType::String,
                    ArrowDt::Boolean => RustDataType::Bool,
                    // 时间戳列固定为 Int64(纳秒),不复用 Arrow Timestamp
                    // 类型(避免纳秒/微秒单位歧义),所以不在此映射
                    _ => RustDataType::String, // 未知类型降级为 String
                };
                PySchemaField {
                    inner: RustSchemaField::new(f.name().clone(), dt),
                }
            })
            .collect()
    }

    /// 零拷贝取单个 batch 为 `pyarrow.RecordBatch`。
    ///
    /// 性能:`pyo3-arrow` 0.16 内部走 `py.import("pyarrow").record_batch()`,
    /// 不复制 Arrow buffer,只 wrap PyObject。
    ///
    /// 错误:`index` 越界返回 `IndexError`。
    fn to_arrow<'py>(&self, py: Python<'py>, index: usize) -> PyResult<Bound<'py, PyAny>> {
        let batch: &RecordBatch = self.inner.batches.get(index).ok_or_else(|| {
            pyo3::exceptions::PyIndexError::new_err(format!(
                "batch index {index} out of range (num_batches={})",
                self.inner.batches.len()
            ))
        })?;
        let py_batch = PyRecordBatch::new(batch.clone());
        // `into_pyarrow` 把 PyRecordBatch 转成 `pyarrow.RecordBatch`
        // (走 pyarrow C-level interop,零数据复制)
        py_batch.into_pyarrow(py)
    }

    /// 一次把所有 batch 拼成 `pyarrow.Table` 返回。
    ///
    /// 用例:Python 端要做全表 filter / agg / 写 parquet 时,`Table` 比
    /// 逐 batch 处理方便。底层仍走 `pyarrow.Table.from_batches`,
    /// batch 本身零拷贝,只是顶层多一层 Table wrapper。
    fn to_arrow_table<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        if self.inner.batches.is_empty() {
            // 空 Dataset → 空 Table(用 schema 构造)
            let locals = pyo3::types::PyDict::new(py);
            let first = PyRecordBatch::new(RecordBatch::new_empty(self.inner.schema.clone()));
            locals.set_item("first", first.into_pyarrow(py)?)?;
            py.run(
                c"import pyarrow as pa; tbl = pa.Table.from_batches([], schema=first.schema)",
                None,
                Some(&locals),
            )
            .map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("pyarrow.Table from empty: {e}"))
            })?;
            return locals
                .get_item("tbl")
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("get tbl: {e}")))?
                .ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err("pyarrow.Table not produced")
                });
        }
        // 非空:走 `Table.from_batches`,传入 schema + batches
        let locals = pyo3::types::PyDict::new(py);
        let batches_py: Vec<Bound<'py, PyAny>> = self
            .inner
            .batches
            .iter()
            .map(|b| {
                let py_batch = PyRecordBatch::new(b.clone());
                py_batch.into_pyarrow(py)
            })
            .collect::<PyResult<Vec<_>>>()?;
        // 把 batches 列表传入 Python
        locals.set_item("batches", batches_py)?;
        // 取 schema:pyarrow.RecordBatch 的 `schema` 是 property(不是 method),
        // 所以用 `getattr` 而不是 `call_method0`,避免 "Schema object is not callable"。
        let schema = self.to_arrow(py, 0)?.getattr("schema")?;
        locals.set_item("schema", schema)?;
        py.run(
            c"import pyarrow as pa; tbl = pa.Table.from_batches(batches, schema=schema)",
            None,
            Some(&locals),
        )
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("pyarrow.Table: {e}")))?;
        locals
            .get_item("tbl")
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("get tbl: {e}")))?
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err("pyarrow.Table not produced"))
    }

    /// 流式迭代为 `Iterator[PyTick]`(Python 端 for-loop 可消费)。
    ///
    /// 返回 `IntoPy<PyObject>` 包装的 Rust 迭代器(`iter_rows`)。
    /// Python 端 `for t in ds.iter_ticks(): ...`。
    fn iter_ticks<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        // 把每个 Tick 转 PyTick,再暴露为 list(简单可靠)。
        // 后续 Stage 1.1 改用 `PyMapping` 或 generator 优化。
        let ticks: Vec<PyTick> = self.inner.iter_rows().map(PyTick::from_tick).collect();
        // 借用 Python list API 把 Vec 转 list
        let list = pyo3::types::PyList::new(py, ticks)?;
        Ok(list.into_any())
    }

    /// 取前 `n` 行,返回新 `Dataset`(共享 Arc 内部所有权)。
    fn take(&self, n: usize) -> Self {
        // Rust `Dataset::take` 返回 owned Dataset,这里 wrap Arc
        Self {
            inner: Arc::new(self.inner.take(n)),
        }
    }

    /// 跳过前 `n` 行,返回新 `Dataset`。
    fn skip(&self, n: usize) -> Self {
        Self {
            inner: Arc::new(self.inner.skip(n)),
        }
    }

    /// 取最后 `n` 行,返回新 `Dataset`。
    fn last_n(&self, n: usize) -> Self {
        Self {
            inner: Arc::new(self.inner.last_n(n)),
        }
    }

    /// 按时间窗口过滤(`start_ts_ns` / `end_ts_ns` 均为纳秒整数,包含两端)。
    ///
    /// 错误:`DataError`(内部错误,如 Arrow compute 失败)→ Python 异常。
    fn by_time_range(&self, start_ts_ns: i64, end_ts_ns: i64) -> PyResult<Self> {
        use crate::error::DataError;
        use axon_core::time::Timestamp;
        let start = Timestamp::from_nanos(start_ts_ns);
        let end = Timestamp::from_nanos(end_ts_ns);
        let filtered = self.inner.by_time_range(start, end).map_err(|e| match e {
            DataError::Internal(msg) => {
                pyo3::exceptions::PyValueError::new_err(format!("by_time_range: {msg}"))
            }
            other => pyo3::exceptions::PyRuntimeError::new_err(format!("by_time_range: {other}")),
        })?;
        Ok(Self {
            inner: Arc::new(filtered),
        })
    }

    /// 数据行 tick 列表(等价 `iter_ticks`,但转 list 一次性返回)。
    ///
    /// Python 端 `for t in ds.ticks(): ...` 或 `ds.ticks()` 取 list。
    fn ticks<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.iter_ticks(py)
    }

    fn __repr__(&self) -> String {
        format!(
            "Dataset(source={}, len={}, num_batches={}, checksum={})",
            self.inner.source,
            self.inner.len(),
            self.inner.batches.len(),
            &self.inner.checksum[..8.min(self.inner.checksum.len())],
        )
    }
}

// `PyTick::from_tick` 是为方便在 `Dataset::iter_ticks` 构造 PyTick;
// `PyTick` 在 `sources.rs` 中已定义,这里加一个 `pub(crate)` 关联函数。
impl PyTick {
    /// 从 Rust `Tick` 构造 `PyTick`(同 crate 内可见)。
    pub(crate) fn from_tick(t: axon_core::market::Tick) -> Self {
        // `PyTick` 字段 `pub inner: Tick`,在同 crate 内可直接构造
        Self { inner: t }
    }
}

/// 在 `_native.data` 子模块下注册 `DataType` + `SchemaField` + `Dataset` 三个类。
///
/// 调用方:`crates/axon-data/src/python/mod.rs::register_module`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyDataType>()?;
    parent.add_class::<PySchemaField>()?;
    parent.add_class::<PyDataset>()?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Frequency;
    use axon_core::market::{Side, Tick};
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity};
    use chrono::{TimeZone, Utc};

    fn utc(y: i32, m: u32, d: u32) -> chrono::DateTime<chrono::Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    fn make_tick(seq: u64) -> Tick {
        Tick::new(
            Timestamp::from_nanos((seq as i64) * 1_000_000_000),
            Price::from_f64(100.0 + seq as f64),
            Quantity::from(1.0),
            Side::Buy,
        )
    }

    fn make_ds(rows: Vec<Tick>) -> Arc<RustDataset> {
        let req =
            RustDataRequest::new("BTCUSDT", utc(2026, 1, 1), utc(2026, 1, 2), Frequency::Tick);
        Arc::new(RustDataset::from_ticks(rows, "test".into(), req).expect("from_ticks"))
    }

    /// `PyDataType::value` 返回与 Rust `DataType` 一致的稳定字符串。
    #[test]
    fn py_datatype_value_matches_rust() {
        assert_eq!(PyDataType::F64.value(), "f64");
        assert_eq!(PyDataType::I64.value(), "i64");
        assert_eq!(PyDataType::Bool.value(), "bool");
        assert_eq!(PyDataType::String.value(), "string");
        assert_eq!(PyDataType::Timestamp.value(), "timestamp");
    }

    /// `PyDataType` 在 Rust ↔ Python 间无信息丢失。
    #[test]
    fn py_datatype_roundtrip() {
        for d in [
            PyDataType::F64,
            PyDataType::I64,
            PyDataType::Bool,
            PyDataType::String,
            PyDataType::Timestamp,
        ] {
            let rust: RustDataType = d.into();
            let back: PyDataType = rust.into();
            assert_eq!(d, back);
        }
    }

    /// `PySchemaField::new` 各 dtype 都能成功构造。
    #[test]
    fn py_schema_field_new_all_dtypes() {
        for s in ["f64", "i64", "bool", "string", "timestamp"] {
            let f = PySchemaField::new("x".into(), s).unwrap();
            assert_eq!(f.name(), "x");
            assert_eq!(f.dtype().value(), s);
        }
    }

    /// `PySchemaField::new` 遇到未知 dtype 报 `ValueError`。
    #[test]
    fn py_schema_field_new_invalid_dtype() {
        let r = PySchemaField::new("x".into(), "u32");
        assert!(r.is_err());
    }

    /// `PyDataset` getter 与 Rust 端一致。
    #[test]
    fn py_dataset_getters_match_rust() {
        let ds = PyDataset {
            inner: make_ds(vec![make_tick(1), make_tick(2), make_tick(3)]),
        };
        assert_eq!(ds.len(), 3);
        assert!(!ds.is_empty());
        assert_eq!(ds.source(), "test");
        assert_eq!(ds.num_batches(), 1);
        assert_eq!(ds.checksum().len(), 64);
    }

    /// `PyDataset::schema()` 返回 4 个固定字段。
    #[test]
    fn py_dataset_schema_has_four_canonical_fields() {
        let ds = PyDataset {
            inner: make_ds(vec![make_tick(1)]),
        };
        let fields = ds.schema();
        assert_eq!(fields.len(), 4);
        let names: Vec<String> = fields.iter().map(|f| f.name()).collect();
        assert_eq!(names, vec!["timestamp", "price", "quantity", "side"]);
    }

    /// `PyDataset::take` / `skip` / `last_n` 与 Rust 端一致。
    #[test]
    fn py_dataset_slice_ops() {
        let ds = PyDataset {
            inner: make_ds((0..5).map(make_tick).collect()),
        };
        assert_eq!(ds.take(3).len(), 3);
        assert_eq!(ds.skip(2).len(), 3);
        assert_eq!(ds.last_n(2).len(), 2);
    }

    /// `PyDataset::iter_ticks` 行数与 `len` 一致。
    #[test]
    fn py_dataset_iter_ticks_count() {
        let ds = PyDataset {
            inner: make_ds((0..4).map(make_tick).collect()),
        };
        Python::attach(|py| {
            let list = ds.iter_ticks(py).unwrap();
            let n: usize = list.call_method0("__len__").unwrap().extract().unwrap();
            assert_eq!(n, 4);
        });
    }

    /// `PyDataset::by_time_range` 过滤后行数正确。
    #[test]
    fn py_dataset_by_time_range_filters() {
        let ds = PyDataset {
            inner: make_ds((0..5).map(make_tick).collect()),
        };
        // ts: 0, 1s, 2s, 3s, 4s (单位:纳秒)
        let filtered = ds
            .by_time_range(1_000_000_000, 3_000_000_000)
            .expect("by_time_range");
        assert_eq!(filtered.len(), 3);
    }

    /// `PyDataset::__repr__` 含关键字段。
    #[test]
    fn py_dataset_repr_contains_fields() {
        let ds = PyDataset {
            inner: make_ds(vec![make_tick(1)]),
        };
        let r = ds.__repr__();
        assert!(r.contains("Dataset"));
        assert!(r.contains("test"));
        assert!(r.contains("len=1"));
    }
}
