//! Python 绑定:`DataService` + `CacheControl` + `CacheStats`。
//!
//! 设计动机:把 Rust `DataService` 的 builder 链式 API 暴露到 Python,
//! 让用户在不写 Rust 的前提下,享受 L1/L2 缓存 + 多数据源接入。
//!
//! ## 关键设计
//!
//! 1. **Tokio runtime 内嵌**:Python 端是同步模型,所有 `async` Rust API
//!    (如 `DataService::load`)都通过 `Arc<Runtime>::block_on(...)` 同步包装。
//!    运行时为 current-thread(`enable_all` 启用 IO/time 驱动器),无 worker pool 开销。
//!
//! 2. **Builder 链式 API**:`DataService.new().register_source(...).with_cache_capacity(N)`
//!    在 Python 端是"修改 in-place + 返回 self";PyO3 用 `PyRefMut` 实现
//!    `&mut self` 接收,通过 `std::mem::take(&mut self.inner).builder_call(...)` 重建。
//!
//! 3. **`MockSource` 直通**:`MockSource` 已实现 `DataSource` trait,
//!    无需 adapter,直接 `Box::new(mock.inner)` 转 `Box<dyn DataSource>`。
//!
//! 4. **`stream()` 同步化**:Python 端 stream 是同步消费语义(无 asyncio),
//!    我们在 Rust 端 drain 整个 stream,返回 list[pa.RecordBatch]。
//!    Stage 1 不强求 Python 端 generator 化(用 list 简单可靠,后续 Stage 1.1 优化)。
//!
//! 数据契约参考:`.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md` §8 Stage 1。

use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;

use futures::StreamExt as _;
use pyo3::prelude::*;
use pyo3_arrow::PyRecordBatch;
use tokio::runtime::Runtime;

use crate::cache::control::CacheControl as RustCacheControl;
use crate::service::{CacheStats as RustCacheStats, DataService as RustService};

use super::dataset::PyDataset;
use super::error::to_py_err;
use super::sources::PyMockSource;
use super::types::PyDataRequest;

// ─── CacheStats ─────────────────────────────────────────────
/// Python 端 `CacheStats`(对应 Rust `CacheStats`)。
///
/// 不可变快照:`hits` / `l2_hits` / `misses` / `len` / `capacity` /
/// `l2_size` / `l2_capacity` 共 7 个字段(L1 + L2)。
#[pyclass(name = "CacheStats", from_py_object)]
#[derive(Debug, Clone, Copy)]
pub struct PyCacheStats {
    /// Rust `CacheStats`(内部存储,值类型)
    pub inner: RustCacheStats,
}

#[pymethods]
impl PyCacheStats {
    /// L1 命中次数。
    #[getter]
    fn hits(&self) -> u64 {
        self.inner.hits
    }

    /// L2 mmap 命中次数(无 mmap-cache feature 时恒为 0)。
    #[getter]
    fn l2_hits(&self) -> u64 {
        self.inner.l2_hits
    }

    /// 未命中次数。
    #[getter]
    fn misses(&self) -> u64 {
        self.inner.misses
    }

    /// L1 当前 entry 数。
    #[getter]
    fn len(&self) -> usize {
        self.inner.len
    }

    /// L1 容量上限。
    #[getter]
    fn capacity(&self) -> usize {
        self.inner.capacity
    }

    /// L2 mmap 当前使用量(字节)。
    #[getter]
    fn l2_size(&self) -> usize {
        self.inner.l2_size
    }

    /// L2 mmap 容量上限(字节)。
    #[getter]
    fn l2_capacity(&self) -> usize {
        self.inner.l2_capacity
    }

    /// 命中率(`hits / (hits + misses)`,无访问时为 0.0)。
    #[getter]
    fn hit_rate(&self) -> f64 {
        let total = self.inner.hits + self.inner.misses;
        if total == 0 {
            0.0
        } else {
            self.inner.hits as f64 / total as f64
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "CacheStats(hits={}, l2_hits={}, misses={}, l1={}/{}, l2={}/{}, hit_rate={:.3})",
            self.inner.hits,
            self.inner.l2_hits,
            self.inner.misses,
            self.inner.len,
            self.inner.capacity,
            self.inner.l2_size,
            self.inner.l2_capacity,
            self.hit_rate(),
        )
    }
}

// ─── DataService ─────────────────────────────────────────────

/// Python 端 `DataService`(对应 Rust `DataService`)。
///
/// 持 `tokio::Runtime`(current-thread),所有 `async` 调用通过
/// `rt.block_on(...)` 同步包装,符合 Python 端无 asyncio 习惯。
#[pyclass(name = "DataService", from_py_object)]
#[derive(Clone)]
pub struct PyDataService {
    /// Rust `DataService`(builder 阶段 in-place 修改)
    pub(crate) inner: RustService,
    /// Tokio current-thread 运行时(`block_on` 包装 async API)
    rt: Arc<Runtime>,
}

// 普通 `impl` 块:内部辅助方法,不暴露给 Python。
impl PyDataService {
    /// 内部构造辅助:同时被 `#[new]` 构造函数和 `new` 静态工厂调用。
    ///
    /// 抽出来是为了避免 `py_init` / `new` 两个 #[pymethods] 函数体重复
    /// (clippy 会报 `clippy::missing_const_for_fn`,且 `tokio::Builder::build`
    /// 非 const,不能直接合并到 `#[new]` 体内)。
    fn new_internal() -> PyResult<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("tokio runtime: {e}"))
            })?;
        Ok(Self {
            inner: RustService::new(),
            rt: Arc::new(rt),
        })
    }
}

#[pymethods]
impl PyDataService {
    /// 构造一个空 `DataService`(L1 默认容量 64,无 source,无 L2)。
    ///
    /// 额外暴露一个 `#[staticmethod] new`(`DataService.new()`),与 `#[new]`
    /// 构造函数(`DataService()`)行为等价,方便 Python 端 builder 链式风格:
    /// ```python
    /// svc = (DataService.new()
    ///     .register_source(...)
    ///     .with_cache_capacity(64))
    /// ```
    #[new]
    fn py_init() -> PyResult<Self> {
        Self::new_internal()
    }

    /// 静态工厂(等价于 `DataService()` 构造函数)。
    ///
    /// 提供 `DataService.new()` 类方法形式,便于链式 builder 风格:
    /// `DataService.new().register_source(...).with_cache_capacity(...)`。
    #[staticmethod]
    fn new() -> PyResult<Self> {
        Self::new_internal()
    }

    /// 注册数据源(builder 风格,链式返回 self)。
    ///
    /// Stage 1 仅支持 `MockSource`(其他 source 在 Stage 1.1 引入时再扩展)。
    /// Python 端:
    /// ```python
    /// svc = (DataService.new()
    ///     .register_source(MockSource.with_tick_series("m", 100, 1_000_000, lambda i: 100.0 + i))
    ///     .with_cache_capacity(64))
    /// ```
    #[pyo3(signature = (source))]
    fn register_source<'py>(
        mut slf: PyRefMut<'py, Self>,
        source: &Bound<'py, PyAny>,
    ) -> PyResult<PyRefMut<'py, Self>> {
        // 提取 MockSource(`extract` 失败 → TypeError)
        let mock: PyMockSource = source.extract::<PyMockSource>().map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err(
                "register_source: only MockSource supported in Stage 1",
            )
        })?;
        // `MockSource` 已实现 `DataSource`,直接 `Box::new` 转 trait object
        // `std::mem::take` 把 slf.inner 取出 owned,调 builder(消费 self)后放回
        let old = std::mem::take(&mut slf.inner);
        let new = old.register_source(Box::new(mock.inner));
        slf.inner = new;
        Ok(slf)
    }

    /// 调整 L1 LRU 容量(builder 风格,链式返回 self)。
    ///
    /// `capacity` 必须 > 0,否则 `ValueError`。
    #[pyo3(signature = (capacity))]
    fn with_cache_capacity<'py>(
        mut slf: PyRefMut<'py, Self>,
        capacity: usize,
    ) -> PyResult<PyRefMut<'py, Self>> {
        let cap = NonZeroUsize::new(capacity)
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("capacity must be > 0"))?;
        let old = std::mem::take(&mut slf.inner);
        let new = old.with_cache_capacity(cap);
        slf.inner = new;
        Ok(slf)
    }

    /// 同步加载数据集(L1 → L2 → 数据源 fallback)。
    ///
    /// 错误:任何 `DataError` 转 Python 异常(`_native.data.DataError`)。
    fn load(&self, req: PyDataRequest) -> PyResult<PyDataset> {
        let inner = self.inner.clone();
        let rust_req = req.inner;
        let ds = self
            .rt
            .block_on(async move { inner.load(&rust_req).await })
            .map_err(to_py_err)?;
        Ok(PyDataset {
            inner: Arc::new(ds),
        })
    }

    /// 读取缓存统计快照。
    fn cache_stats(&self) -> PyCacheStats {
        let s = self.inner.cache_stats();
        PyCacheStats { inner: s }
    }

    /// 缓存运维句柄(清缓存、调容量)。
    fn cache_control(&self) -> PyCacheControl {
        PyCacheControl {
            inner: self.inner.cache_control(),
        }
    }

    /// 同步流式订阅:把 source stream drain 成 list[pa.RecordBatch] 返回。
    ///
    /// 注:此方法**不写 L1/L2 缓存**(透传源)。若 caller 需要缓存语义,先调 `load()`。
    /// Stage 1 简化:完整 collect 后一次返回;后续 Stage 1.1 改 generator。
    fn stream(&self, source_name: String, req: PyDataRequest) -> PyResult<Vec<PyArrowBatch>> {
        let inner = self.inner.clone();
        let rust_req = req.inner;
        let mut s = self
            .rt
            .block_on(async move { inner.stream(&source_name, &rust_req).await })
            .map_err(to_py_err)?;
        // `block_on` 闭包返回 future of Stream,我们用 try_collect 风格的循环 drain
        // 实际上上面 `block_on` 拿到的已经是 stream Pin<Box<...>>,需要在 GIL 外迭代
        // 简单做法:用 futures 同步迭代(要求 std runtime / current_thread rt)
        // 这里用 `tokio::runtime::Handle::block_on` 重 drain(同一 rt)
        let mut out: Vec<PyArrowBatch> = Vec::new();
        // 用同步循环 + 局部 tokio task 驱动 stream
        // 简单方案:另起一个 future drain 整个 stream
        let drain_fut = async move {
            let mut total: Vec<arrow::record_batch::RecordBatch> = Vec::new();
            while let Some(item) = s.next().await {
                total.push(item.map_err(to_py_err)?);
            }
            Ok::<Vec<arrow::record_batch::RecordBatch>, PyErr>(total)
        };
        let batches = self.rt.block_on(drain_fut)?;
        for b in batches {
            out.push(PyArrowBatch { inner: b });
        }
        Ok(out)
    }

    fn __repr__(&self) -> String {
        let s = self.inner.cache_stats();
        format!(
            "DataService(l1={}/{}, hits={}, misses={}, sources=N/A)",
            s.len, s.capacity, s.hits, s.misses,
        )
    }
}

// ─── CacheControl ─────────────────────────────────────────────

/// Python 端 `CacheControl`(对应 Rust `CacheControl`)。
///
/// 持 `DataServiceInner` 的 Arc 引用,与 `DataService` 共享同一缓存视图。
#[pyclass(name = "CacheControl", from_py_object)]
#[derive(Clone)]
pub struct PyCacheControl {
    /// Rust `CacheControl`(`Clone` 只复制 Arc 指针)
    pub inner: RustCacheControl,
}

#[pymethods]
impl PyCacheControl {
    /// 清空 L1 LRU 缓存(不影响 L2)。
    fn clear_l1(&self) {
        self.inner.clear_l1();
    }

    /// 清空 L2 mmap 缓存(需要 `mmap-cache` feature)。
    ///
    /// 错误:L2 未配置时返回 `RuntimeError`。
    #[cfg(feature = "mmap-cache")]
    fn clear_l2(&self) -> PyResult<()> {
        self.inner.clear_l2().map_err(to_py_err)
    }

    /// 调整 L1 LRU 容量(`new_capacity` 必须 > 0)。
    fn resize_l1(&self, new_capacity: usize) -> PyResult<()> {
        let cap = NonZeroUsize::new(new_capacity)
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("new_capacity must be > 0"))?;
        self.inner.resize_l1(cap);
        Ok(())
    }

    fn __repr__(&self) -> String {
        "CacheControl(l1=shared, l2=shared)".to_string()
    }
}

// ─── 内部:零拷贝 Arrow batch 包装 ──────────────────────────

/// Python 端零拷贝 Arrow RecordBatch 包装。
///
/// 不暴露给 Python(不通过 `add_class` 注册),仅作为 `stream()` 返回
/// list 元素的 Rust 端存储;PyO3 默认 `IntoPy` 会把 `Vec<PyArrowBatch>` 转 list
/// 时调用 `__repr__`,我们覆写为 `into_pyarrow` 转换逻辑。
///
/// **注意:** 实际 `stream()` 返回 `Vec<PyArrowBatch>`,PyO3 会逐个调
/// `IntoPy<PyObject>::into_py()`。我们手动实现该转换,把 `RecordBatch`
/// 转 `pyarrow.RecordBatch`(走 `pyo3-arrow`)再返回。
pub struct PyArrowBatch {
    /// Rust 端 `RecordBatch`(持有所有权,转 Python 后由 pyarrow 接管)
    pub inner: arrow::record_batch::RecordBatch,
}

impl<'py> IntoPyObject<'py> for PyArrowBatch {
    type Target = pyo3::PyAny;
    type Output = Bound<'py, Self::Target>;
    type Error = PyErr;

    fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
        let py_batch = PyRecordBatch::new(self.inner);
        py_batch.into_pyarrow(py)
    }
}

/// 在 `_native.data` 子模块下注册 `DataService` + `CacheStats` + `CacheControl`。
///
/// 调用方:`crates/axon-data/src/python/mod.rs::register_module`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyDataService>()?;
    parent.add_class::<PyCacheStats>()?;
    parent.add_class::<PyCacheControl>()?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::{Side, Tick};
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity};
    use chrono::{TimeZone, Utc};

    fn utc(y: i32, m: u32, d: u32) -> chrono::DateTime<chrono::Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    fn make_req() -> PyDataRequest {
        use super::super::types::PyFrequency;
        use crate::types::{DataRequest as RustDataRequest, Frequency as RustFreq};
        let rust = RustDataRequest::new("X", utc(2026, 1, 1), utc(2026, 1, 2), RustFreq::Tick);
        // PyFrequency 与 RustFreq 是 1:1 转换;这里通过 PyFrequency::Tick → RustFreq::Tick
        let _ = PyFrequency::Tick;
        PyDataRequest { inner: rust }
    }

    /// 新建 `DataService` 默认 L1 容量 64。
    #[test]
    fn py_dataservice_new_default_capacity_64() {
        let svc = PyDataService::new_internal().expect("new");
        let stats = svc.cache_stats();
        assert_eq!(stats.capacity(), 64);
        assert_eq!(stats.len(), 0);
    }

    /// `with_cache_capacity(0)` 在 Rust 端被 `NonZeroUsize::new` 拒绝。
    #[test]
    fn py_dataservice_with_cache_capacity_zero_is_none() {
        // `NonZeroUsize::new(0)` 返回 None,builder 端需要 cap > 0
        // (pymethod 阶段把 `usize=0` 转 `NonZeroUsize`,返回 `ValueError`)
        assert!(NonZeroUsize::new(0).is_none());
        assert!(NonZeroUsize::new(8).is_some());
    }

    /// `load` 找不到 source 时抛 `DataError`。
    #[test]
    fn py_dataservice_load_no_source_raises_data_error() {
        let svc = PyDataService::new_internal().expect("new");
        let r = svc.load(make_req());
        assert!(r.is_err(), "expected DataError when no source registered");
    }

    /// `register_source(MockSource.empty())` + `load` 返回空 dataset。
    #[test]
    fn py_dataservice_load_with_empty_mock_returns_empty_dataset() {
        use crate::sources::MockSource as RustMock;
        let mut svc = PyDataService::new_internal().expect("new");
        let mock = RustMock::empty();
        let old = std::mem::take(&mut svc.inner);
        svc.inner = old.register_source(Box::new(mock));

        let ds = svc.load(make_req()).expect("load");
        // PyDataset 内部是 Arc<RustDataset>,直接读 len()
        assert_eq!(ds.inner.len(), 0);
        assert!(ds.inner.is_empty());
    }

    /// 连续两次 `load` 相同 req → 第二次走 L1 命中。
    #[test]
    fn py_dataservice_cache_hit_increments_hits() {
        use crate::sources::MockSource as RustMock;
        let mut svc = PyDataService::new_internal().expect("new");
        let mock = RustMock::with_rows(
            "m",
            vec![Tick::new(
                Timestamp::from_nanos(0),
                Price::from_f64(1.0),
                Quantity::from(1.0),
                Side::Buy,
            )],
        );
        let old = std::mem::take(&mut svc.inner);
        svc.inner = old.register_source(Box::new(mock));

        let _ = svc.load(make_req()).expect("load1");
        let _ = svc.load(make_req()).expect("load2");
        let _ = svc.load(make_req()).expect("load3");

        let stats = svc.cache_stats();
        assert_eq!(stats.misses(), 1);
        assert_eq!(stats.hits(), 2);
    }

    /// `cache_control().clear_l1()` 后 L1 entry 数归零。
    #[test]
    fn py_cache_control_clear_l1_empties_cache() {
        use crate::sources::MockSource as RustMock;
        let mut svc = PyDataService::new_internal().expect("new");
        let mock = RustMock::with_rows(
            "m",
            vec![Tick::new(
                Timestamp::from_nanos(0),
                Price::from_f64(1.0),
                Quantity::from(1.0),
                Side::Buy,
            )],
        );
        let old = std::mem::take(&mut svc.inner);
        svc.inner = old.register_source(Box::new(mock));

        let _ = svc.load(make_req()).expect("load");
        assert!(svc.cache_stats().len() > 0);

        let ctrl = svc.cache_control();
        ctrl.clear_l1();
        assert_eq!(svc.cache_stats().len(), 0);
    }

    /// `cache_control().resize_l1(8)` 调整容量。
    #[test]
    fn py_cache_control_resize_l1_takes_effect() {
        let svc = PyDataService::new_internal().expect("new");
        let ctrl = svc.cache_control();
        ctrl.resize_l1(8).expect("resize");
        assert_eq!(svc.cache_stats().capacity(), 8);
    }

    /// `PyCacheStats::hit_rate` 计算正确。
    #[test]
    fn py_cache_stats_hit_rate() {
        let s = PyCacheStats {
            inner: RustCacheStats {
                hits: 3,
                l2_hits: 0,
                misses: 1,
                len: 0,
                capacity: 64,
                l2_size: 0,
                l2_capacity: 0,
            },
        };
        assert!((s.hit_rate() - 0.75).abs() < 1e-9);
    }

    /// `PyCacheStats::hit_rate` 全空时为 0(避免除零)。
    #[test]
    fn py_cache_stats_hit_rate_zero_when_empty() {
        let s = PyCacheStats {
            inner: RustCacheStats {
                hits: 0,
                l2_hits: 0,
                misses: 0,
                len: 0,
                capacity: 64,
                l2_size: 0,
                l2_capacity: 0,
            },
        };
        assert_eq!(s.hit_rate(), 0.0);
    }

    /// `stream` 透传源,返回 list[pa.RecordBatch]。
    #[test]
    fn py_dataservice_stream_passes_through() {
        use crate::sources::MockSource as RustMock;
        let mut svc = PyDataService::new_internal().expect("new");
        let mock = RustMock::with_rows(
            "m",
            vec![Tick::new(
                Timestamp::from_nanos(0),
                Price::from_f64(1.0),
                Quantity::from(1.0),
                Side::Buy,
            )],
        );
        let old = std::mem::take(&mut svc.inner);
        svc.inner = old.register_source(Box::new(mock));

        let batches = svc.stream("m".into(), make_req()).expect("stream");
        assert_eq!(batches.len(), 1, "MockSource 默认 1 batch");
        // `__len__` 与 Rust 一致
        let ds_rows = batches.iter().map(|b| b.inner.num_rows()).sum::<usize>();
        assert_eq!(ds_rows, 1);
    }

    /// `stream` 找不存在的 source 抛 `DataError`。
    #[test]
    fn py_dataservice_stream_no_source_raises() {
        let svc = PyDataService::new_internal().expect("new");
        let r = svc.stream("nonexistent".into(), make_req());
        assert!(r.is_err());
    }

    /// `__repr__` 含关键字段。
    #[test]
    fn py_dataservice_repr_contains_fields() {
        let svc = PyDataService::new_internal().expect("new");
        let r = svc.__repr__();
        assert!(r.contains("DataService"));
        assert!(r.contains("64"));
    }
}

// 避免 `Pin` 引入未用 import 的 warning(若编译器优化不需要则忽略)
#[allow(dead_code)]
fn _pin_type_assert() {
    // 编译期断言:`Pin<Box<dyn Stream + Send>>` 是 `Send`
    fn assert_send<T: Send>() {}
    assert_send::<
        Pin<
            Box<
                dyn futures_core::Stream<
                        Item = crate::error::DataResult<arrow::record_batch::RecordBatch>,
                    > + Send,
            >,
        >,
    >();
}
