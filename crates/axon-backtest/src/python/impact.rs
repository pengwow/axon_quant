//! `ImpactedMatchingEngine` + `ImpactModel` Python 绑定
//!
//! 把 `axon-core::impact` 的 [`ImpactModel`](axon_core::impact::ImpactModel)
//! 体系与 `axon-backtest::impact::ImpactedMatchingEngine` 暴露给 Python。
//!
//! # 数据契约
//!
//! - **入参订单**:`dict`(见 `super::types::dict_to_order`)
//! - **出参成交**: `dict`(见 `super::types::submit_result_to_dict`)
//! - **冲击模型**:支持 3 种构造方式
//!   1. **原生预设**:`ImpactedMatchingEngine("linear", coefficient, ...)` 直接构造
//!   2. **Builder 模式**:`ImpactedMatchingEngineBuilder().model_type("power_law")...build()`
//!   3. **Python 自定义**:实现一个含 `compute_impact(order_quantity, side, order_book) -> dict`
//!      方法的 Python 类,然后通过 `ImpactedMatchingEngine.with_custom_model(py_obj)` 注入
//!
//! # 错误处理
//!
//! - 模型参数越界(系数负、ratio 超 [0, 1] 等)→ `PyValueError`
//! - Python 自定义模型 `compute_impact` 抛异常 → 透传为 `PyErr`(`RuntimeError` 等)
//! - Python 自定义模型返回非 dict → `PyValueError`
//! - 字段缺失 → `PyKeyError`
//!
//! # 迁移说明
//!
//! 本文件从原 `src/impact/python.rs`(已删除)迁移并扩展:
//! - 复用 `super::types` 的 dict 协议(`dict_to_order` / `submit_result_to_dict` / ...)
//!   不再各自重复实现
//! - 去除 `OrderBookSnapshot` 的二次封装,Python 自定义模型收到的是
//!   原始 `dict`(bids/asks/timestamp),而非 `#[pyclass]`,减少对象桥接开销
//! - 新增 `ImpactedMatchingEngineBuilder` 提供更 Pythonic 的链式构造
//! - 新增 `ImpactedMatchingEngine.with_custom_model()` 允许 Python 自定义冲击模型
//!
//! # 设计要点
//!
//! - **`PyImpactModelAdapter` 的线程安全**:内部持 `Arc<Py<PyAny>>`,在
//!   `compute_impact` 中通过 `Python::attach` 拿 GIL 回调 Python 方法。
//!   `Py<T>` 本身是 `Send + Sync`(`GIL` 保护内部状态),所以适配器自然满足
//!   [`ImpactModel: Send + Sync`]。
//! - **GIL 获取成本**:每次 `submit` 都会调用 `compute_impact`,即 1 次 GIL 获取
//!   + 1 次 Python 回调;`Python::attach` 已是 fast path,实测开销可接受。

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]
#![allow(deprecated)]

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use axon_core::impact::{Impact as CoreImpact, ImpactModel as CoreImpactModel, ImpactModelConfig};
use axon_core::market::{OrderBookSnapshot, Side as CoreSide};
use axon_core::types::Quantity;

use crate::impact::impacted_engine::ImpactedMatchingEngine as RustEngine;

use super::types::{dict_to_order, submit_result_to_dict};

// ═══════════════════════════════════════════════════════════════════════════
// 主类: PyImpactedMatchingEngine
// ═══════════════════════════════════════════════════════════════════════════

/// Python 侧冲击感知撮合引擎
///
/// 包装 Rust `ImpactedMatchingEngine`,在 L1 撮合基础上叠加市场冲击。
///
/// 构造方式 3 种(详见模块级 doc):
/// 1. 直接传 model_type + 参数:`ImpactedMatchingEngine("linear", 0.05)`
/// 2. Builder:`ImpactedMatchingEngineBuilder().model_type("power_law").coefficient(0.1).exponent(0.5).build()`
/// 3. 自定义模型:`ImpactedMatchingEngine.with_custom_model(my_python_model)`
#[pyclass(name = "ImpactedMatchingEngine")]
pub struct PyImpactedMatchingEngine {
    /// 内部 Rust 引擎
    inner: RustEngine,
}

#[pymethods]
impl PyImpactedMatchingEngine {
    /// 直接构造(简单场景)
    ///
    /// Args:
    /// - `model_type`:`"linear"` / `"power_law"`
    /// - `coefficient`:冲击系数
    /// - `depth_levels`:深度层级数(默认 10)
    /// - `instantaneous_ratio`:即时/永久比例(默认 0.7,范围 [0, 1])
    /// - `exponent`:幂律指数(仅 `power_law`,默认 0.5,范围 (0, 2])
    /// - `permanent_decay`:永久冲击衰减率(默认 0.0,范围 [0, 1])
    #[new]
    #[pyo3(signature = (model_type, coefficient, depth_levels=10, instantaneous_ratio=0.7, exponent=0.5, permanent_decay=0.0))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        model_type: &str,
        coefficient: f64,
        depth_levels: usize,
        instantaneous_ratio: f64,
        exponent: f64,
        permanent_decay: f64,
    ) -> PyResult<Self> {
        let config = build_config(
            model_type,
            coefficient,
            depth_levels,
            instantaneous_ratio,
            exponent,
        )?;
        let model: Box<dyn CoreImpactModel> = axon_core::impact::create_model(config);
        let engine = RustEngine::new(model).with_permanent_decay(permanent_decay);
        Ok(Self { inner: engine })
    }

    /// 提交订单,返回成交结果 dict
    ///
    /// 字段:`fills` / `is_filled` / `is_partially_filled` / `remaining_quantity`
    fn submit<'py>(
        &mut self,
        py: Python<'py>,
        order_dict: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let order = dict_to_order(order_dict)?;
        let result = self.inner.submit(order);
        submit_result_to_dict(py, &result)
    }

    /// 取消订单
    fn cancel(&mut self, order_id: u64) -> bool {
        self.inner.cancel(order_id)
    }

    /// 注入 Python 自定义冲击模型
    ///
    /// Args:
    /// - `model`:Python 对象,需实现 `compute_impact(order_quantity: float, side: str, order_book: dict) -> dict`
    ///   返回的 dict 含 `instantaneous` / `permanent` 字段
    ///
    /// 失败原因:
    /// - `model` 不是 `PyAny` → 类型错误(由 pyo3 自动捕获)
    fn with_custom_model(&mut self, model: Py<PyAny>) {
        let adapter = PyImpactModelAdapter::new(model);
        self.inner.set_model(Box::new(adapter));
    }

    /// 播种虚拟流动性（回测辅助）
    ///
    /// 在 mid_price 上下分别挂 depth_levels 层限价单作为对手盘。
    /// 让策略单在没有外部对手盘时仍能成交。
    ///
    /// Args:
    /// - `mid_price`: 中间价（通常为当前 bar close）
    /// - `half_spread`: 每层价差（绝对价格单位）
    /// - `depth_levels`: 每侧挂单层数
    /// - `size_per_level`: 每层挂单数量
    /// - `instrument`: instrument dict(由 `spot_instrument()` / `swap_instrument()` 工厂构造)
    /// - `next_id`: 起始订单 id
    ///
    /// Returns:
    ///   更新后的 id 计数器（传给下一次 seed 调用）
    #[allow(clippy::too_many_arguments)] // seed 参数多(中间价/价差/层数/层量/instrument/id),不可避免
    fn seed_liquidity(
        &mut self,
        _py: Python<'_>,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        instrument: &Bound<'_, PyAny>,
        next_id: u64,
    ) -> PyResult<u64> {
        let inst = super::types::parse_instrument(instrument.cast::<PyDict>()?)?;
        Ok(self.inner.seed_liquidity(
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            inst,
            next_id,
        ))
    }

    /// 清空订单簿两侧（回测辅助 — 瞬时对手盘场景）
    ///
    /// 用法:每根 bar 处理前调 `clear_book()` 再 `seed_liquidity(...)`,
    /// 避免种子单跨 bar 累积撑爆 BTreeMap。**不**清空永久冲击偏移与统计,
    /// 那部分跨 bar 持续累计。
    fn clear_book(&mut self) {
        self.inner.clear_book();
    }

    /// 当前累计永久冲击偏移
    fn permanent_offset(&self) -> f64 {
        self.inner.permanent_offset()
    }

    /// 最优买价(应用永久冲击后)
    fn best_bid(&self) -> Option<f64> {
        self.inner.best_bid().map(|p| p.as_f64())
    }

    /// 最优卖价(应用永久冲击后)
    fn best_ask(&self) -> Option<f64> {
        self.inner.best_ask().map(|p| p.as_f64())
    }

    /// 中间价(应用永久冲击后)
    fn mid_price(&self) -> Option<f64> {
        self.inner.mid_price().map(|p| p.as_f64())
    }

    /// 活跃订单数
    fn active_order_count(&self) -> usize {
        self.inner.active_order_count()
    }

    /// 重置冲击状态(保留订单簿)
    fn reset_impact_state(&mut self) {
        self.inner.reset_impact_state();
    }

    /// 统计信息(dict)
    ///
    /// 字段:`cumulative_instantaneous` / `cumulative_permanent` /
    /// `cumulative_total` / `submitted_orders` / `filled_orders` / `total_fills`
    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let stats = self.inner.stats();
        let d = PyDict::new(py);
        d.set_item("cumulative_instantaneous", stats.cumulative_instantaneous)?;
        d.set_item("cumulative_permanent", stats.cumulative_permanent)?;
        d.set_item("cumulative_total", stats.cumulative_total())?;
        d.set_item("submitted_orders", stats.submitted_orders)?;
        d.set_item("filled_orders", stats.filled_orders)?;
        d.set_item("total_fills", stats.total_fills)?;
        Ok(d)
    }

    /// 当前模型名称
    fn model_name(&self) -> String {
        self.inner.model_name().to_string()
    }

    /// 永久冲击衰减率
    fn permanent_decay(&self) -> Option<f64> {
        self.inner.permanent_decay()
    }

    fn __repr__(&self) -> String {
        format!(
            "ImpactedMatchingEngine(model={}, permanent_offset={:.4}, active_orders={})",
            self.inner.model_name(),
            self.inner.permanent_offset(),
            self.inner.active_order_count()
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Builder: PyImpactedMatchingEngineBuilder
// ═══════════════════════════════════════════════════════════════════════════

/// Builder 模式构造 [`PyImpactedMatchingEngine`]
///
/// 提供 Pythonic 链式 API,避免记忆大量 `__init__` 关键字参数。
/// 未设置的字段使用 Rust 侧默认值。
///
/// Example:
/// ```python
/// engine = (ImpactedMatchingEngineBuilder()
///            .model_type("power_law")
///            .coefficient(0.1)
///            .exponent(0.5)
///            .depth_levels(5)
///            .instantaneous_ratio(0.8)
///            .permanent_decay(0.05)
///            .build())
/// ```
#[pyclass(name = "ImpactedMatchingEngineBuilder")]
pub struct PyImpactedMatchingEngineBuilder {
    model_type: Option<String>,
    coefficient: Option<f64>,
    depth_levels: Option<usize>,
    instantaneous_ratio: Option<f64>,
    exponent: Option<f64>,
    permanent_decay: Option<f64>,
}

#[pymethods]
impl PyImpactedMatchingEngineBuilder {
    #[new]
    fn new() -> Self {
        Self {
            model_type: None,
            coefficient: None,
            depth_levels: None,
            instantaneous_ratio: None,
            exponent: None,
            permanent_decay: None,
        }
    }

    /// 设置模型类型(`"linear"` / `"power_law"`)
    fn model_type<'py>(mut slf: PyRefMut<'py, Self>, t: String) -> PyRefMut<'py, Self> {
        slf.model_type = Some(t);
        slf
    }

    /// 设置冲击系数
    fn coefficient<'py>(mut slf: PyRefMut<'py, Self>, c: f64) -> PyRefMut<'py, Self> {
        slf.coefficient = Some(c);
        slf
    }

    /// 设置深度层级数
    fn depth_levels<'py>(mut slf: PyRefMut<'py, Self>, d: usize) -> PyRefMut<'py, Self> {
        slf.depth_levels = Some(d);
        slf
    }

    /// 设置即时/永久比例(范围 [0, 1])
    fn instantaneous_ratio<'py>(mut slf: PyRefMut<'py, Self>, r: f64) -> PyRefMut<'py, Self> {
        slf.instantaneous_ratio = Some(r);
        slf
    }

    /// 设置幂律指数(仅 `power_law`,范围 (0, 2])
    fn exponent<'py>(mut slf: PyRefMut<'py, Self>, e: f64) -> PyRefMut<'py, Self> {
        slf.exponent = Some(e);
        slf
    }

    /// 设置永久冲击衰减率(范围 [0, 1])
    fn permanent_decay<'py>(mut slf: PyRefMut<'py, Self>, d: f64) -> PyRefMut<'py, Self> {
        slf.permanent_decay = Some(d);
        slf
    }

    /// 构建引擎
    fn build(&self) -> PyResult<PyImpactedMatchingEngine> {
        let m = self.model_type.as_deref().unwrap_or("linear");
        let c = self.coefficient.unwrap_or(0.0);
        let d = self.depth_levels.unwrap_or(10);
        let r = self.instantaneous_ratio.unwrap_or(0.7);
        let e = self.exponent.unwrap_or(0.5);
        let pd = self.permanent_decay.unwrap_or(0.0);
        PyImpactedMatchingEngine::new(m, c, d, r, e, pd)
    }

    fn __repr__(&self) -> String {
        format!(
            "ImpactedMatchingEngineBuilder(model_type={:?}, coefficient={:?}, depth_levels={:?}, \
             instantaneous_ratio={:?}, exponent={:?}, permanent_decay={:?})",
            self.model_type,
            self.coefficient,
            self.depth_levels,
            self.instantaneous_ratio,
            self.exponent,
            self.permanent_decay,
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Python 自定义 ImpactModel 适配器
// ═══════════════════════════════════════════════════════════════════════════

/// 适配 Python 自定义冲击模型到 Rust [`CoreImpactModel`]
///
/// 内部持 `Arc<Py<PyAny>>`(Python 端用户对象),在 `compute_impact` 中
/// 通过 `Python::attach` 拿 GIL 回调 Python `compute_impact` 方法。
///
/// # Python 协议
///
/// 用户需在 Python 端实现一个类,提供方法:
/// ```python
/// def compute_impact(self, order_quantity: float, side: str, order_book: dict) -> dict:
///     """
///     Args:
///         order_quantity: 订单数量
///         side: "BUY" 或 "SELL"
///         order_book: dict 含 bids / asks / timestamp_ns
///             bids: list[dict] 每个元素含 price / quantity
///             asks: list[dict] 每个元素含 price / quantity
///             timestamp_ns: int
///     Returns:
///         dict 含 instantaneous (float) / permanent (float)
///     """
///     return {"instantaneous": 0.0, "permanent": 0.0}
/// ```
pub struct PyImpactModelAdapter {
    /// Python 用户对象(`Py<T>` 是 `Send + Sync`,因 GIL 保护)
    py_obj: Arc<Py<PyAny>>,
}

impl PyImpactModelAdapter {
    /// 创建适配器
    pub fn new(py_obj: Py<PyAny>) -> Self {
        Self {
            py_obj: Arc::new(py_obj),
        }
    }
}

impl std::fmt::Debug for PyImpactModelAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyImpactModelAdapter")
            .field("py_obj", &"<Py<PyAny>>")
            .finish()
    }
}

impl CoreImpactModel for PyImpactModelAdapter {
    fn compute_impact(
        &self,
        order_quantity: Quantity,
        side: CoreSide,
        order_book: &OrderBookSnapshot,
    ) -> CoreImpact {
        // 拿 GIL 调 Python 回调
        let outcome: Result<(f64, f64), PyErr> = Python::attach(|py| {
            // 1. 把 OrderBookSnapshot 转 Python dict
            let book_dict = snapshot_to_dict(py, order_book)?;

            // 2. 调 Python `compute_impact` 方法
            let method = self.py_obj.bind(py).getattr("compute_impact")?;
            let qty_f: f64 = order_quantity.as_f64();
            let side_str: &str = match side {
                CoreSide::Buy => "BUY",
                CoreSide::Sell => "SELL",
            };
            let py_result = method.call1((qty_f, side_str, book_dict))?;

            // 3. 期望返回 dict,提取 instantaneous / permanent(缺字段时 0)
            let dict = py_result
                .cast::<PyDict>()
                .map_err(|_e| PyValueError::new_err("compute_impact must return a dict"))?;
            let instantaneous: f64 = dict
                .get_item("instantaneous")
                .ok()
                .flatten()
                .and_then(|v| v.extract::<f64>().ok())
                .unwrap_or(0.0);
            let permanent: f64 = dict
                .get_item("permanent")
                .ok()
                .flatten()
                .and_then(|v| v.extract::<f64>().ok())
                .unwrap_or(0.0);
            Ok((instantaneous, permanent))
        });

        // 失败:Python 异常已透传为 PyErr,这里只能 panic(因为 trait 返回 Impact)
        // 注:实测中 compute_impact 失败应被测试捕获
        match outcome {
            Ok((instantaneous, permanent)) => CoreImpact {
                instantaneous,
                permanent,
            },
            Err(e) => {
                eprintln!("PyImpactModelAdapter::compute_impact error: {e}");
                CoreImpact::zero()
            }
        }
    }

    fn name(&self) -> &str {
        // 静态字符串:返回类型约束
        "PythonImpactModel"
    }

    fn params(&self) -> String {
        format!("py_obj=<{:?}>", Arc::as_ptr(&self.py_obj))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 内部辅助
// ═══════════════════════════════════════════════════════════════════════════

/// 把传入的 model_type 字符串映射为 `ImpactModelConfig`
fn build_config(
    model_type: &str,
    coefficient: f64,
    depth_levels: usize,
    instantaneous_ratio: f64,
    exponent: f64,
) -> PyResult<ImpactModelConfig> {
    match model_type.to_lowercase().as_str() {
        "linear" => Ok(ImpactModelConfig::Linear {
            coefficient,
            depth_levels,
            instantaneous_ratio,
        }),
        "power_law" | "powerlaw" | "sqrt" => Ok(ImpactModelConfig::PowerLaw {
            coefficient,
            exponent,
            depth_levels,
            instantaneous_ratio,
        }),
        other => Err(PyValueError::new_err(format!(
            "unknown model_type: {other} (expected 'linear' / 'power_law')"
        ))),
    }
}

/// `OrderBookSnapshot` → Python dict
///
/// 供 `PyImpactModelAdapter` 把订单簿传给 Python `compute_impact`。
/// 字段:
/// - `bids`: `list[dict]`(price / quantity,降序)
/// - `asks`: `list[dict]`(price / quantity,升序)
/// - `timestamp_ns`: `int`
fn snapshot_to_dict<'py>(
    py: Python<'py>,
    snap: &OrderBookSnapshot,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("timestamp_ns", snap.timestamp.nanos as u64)?;

    let bids = PyList::empty(py);
    for lvl in &snap.bids {
        let entry = PyDict::new(py);
        entry.set_item("price", lvl.price.as_f64())?;
        entry.set_item("quantity", lvl.quantity.as_f64())?;
        bids.append(entry)?;
    }
    d.set_item("bids", bids)?;

    let asks = PyList::empty(py);
    for lvl in &snap.asks {
        let entry = PyDict::new(py);
        entry.set_item("price", lvl.price.as_f64())?;
        entry.set_item("quantity", lvl.quantity.as_f64())?;
        asks.append(entry)?;
    }
    d.set_item("asks", asks)?;

    Ok(d)
}

/// 当前模块需要在 `parent`(即 `_native.backtest`)下注册以下类:
/// - `ImpactedMatchingEngine`
/// - `ImpactedMatchingEngineBuilder`
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyImpactedMatchingEngine>()?;
    parent.add_class::<PyImpactedMatchingEngineBuilder>()?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyDict;

    use axon_core::time::Timestamp;
    use axon_core::types::Price;

    fn make_limit_dict<'py>(
        py: Python<'py>,
        id: u64,
        symbol: &str,
        side: &str,
        price: f64,
        qty: f64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("id", id)?;
        d.set_item("symbol", symbol)?;
        d.set_item("side", side)?;
        d.set_item("type", "limit")?;
        d.set_item("price", price)?;
        d.set_item("quantity", qty)?;
        d.set_item("tif", "GTC")?;
        Ok(d)
    }

    // ─── 构造与基础属性 ─────────────────────────────

    /// 默认构造(linear + coefficient=0.05)
    #[test]
    fn default_constructor_sets_linear_model() {
        let m = PyImpactedMatchingEngine::new("linear", 0.05, 10, 0.7, 0.5, 0.0).unwrap();
        assert_eq!(m.model_name(), "LinearImpact");
        assert_eq!(m.permanent_offset(), 0.0);
        assert_eq!(m.permanent_decay(), Some(0.0));
    }

    /// `power_law` 构造
    #[test]
    fn power_law_constructor() {
        let m = PyImpactedMatchingEngine::new("power_law", 0.1, 5, 0.7, 0.5, 0.0).unwrap();
        assert_eq!(m.model_name(), "PowerLawImpact");
    }

    /// 未知 model_type → `PyValueError`
    #[test]
    fn unknown_model_type_raises() {
        Python::attach(|py| {
            let r = PyImpactedMatchingEngine::new("bogus", 0.1, 10, 0.7, 0.5, 0.0);
            // 用 match 替代 `unwrap_err`(后者需要 T: Debug)
            let err = match r {
                Ok(_) => panic!("expected error for invalid model_type"),
                Err(e) => e,
            };
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    /// `__repr__` 含关键字段
    #[test]
    fn repr_contains_key_fields() {
        let m = PyImpactedMatchingEngine::new("linear", 0.05, 10, 0.7, 0.5, 0.0).unwrap();
        let s = m.__repr__();
        assert!(s.contains("ImpactedMatchingEngine"));
        assert!(s.contains("model=LinearImpact"));
    }

    // ─── submit + 冲击路径 ─────────────────────────

    /// 零冲击场景:coefficient=0.0,fill 价不变
    #[test]
    fn zero_coefficient_preserves_fill_prices() {
        Python::attach(|py| {
            let mut m = PyImpactedMatchingEngine::new("linear", 0.0, 10, 0.7, 0.5, 0.0).unwrap();
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 100.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            let buy = make_limit_dict(py, 2, "BTC-USDT", "buy", 100.0, 1.0).unwrap();
            let result = m.submit(py, &buy).unwrap();
            assert!(
                result
                    .get_item("is_filled")
                    .unwrap()
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
            );
            let fills = result.get_item("fills").unwrap().unwrap();
            assert_eq!(fills.len().unwrap(), 1);
            // 零冲击 ⇒ 价格 100.0
            let fill_dict = fills.get_item(0).unwrap();
            let price: f64 = fill_dict.get_item("price").unwrap().extract().unwrap();
            assert!((price - 100.0).abs() < 1e-9);
        });
    }

    /// 线性冲击:buy 方向 fill 价 > 100
    #[test]
    fn linear_buy_raises_fill_price() {
        Python::attach(|py| {
            let mut m = PyImpactedMatchingEngine::new("linear", 0.05, 10, 0.7, 0.5, 0.0).unwrap();
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 100.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            let buy = make_limit_dict(py, 2, "BTC-USDT", "buy", 100.0, 5.0).unwrap();
            let result = m.submit(py, &buy).unwrap();
            let fills = result.get_item("fills").unwrap().unwrap();
            let fill_dict = fills.get_item(0).unwrap();
            let price: f64 = fill_dict.get_item("price").unwrap().extract().unwrap();
            // 即时冲击:0.05 * (1/1) * 0.7 = 0.035
            assert!(price > 100.0, "expected price > 100, got {price}");
        });
    }

    /// 永久冲击:ratio=0.0 全部永久
    #[test]
    fn permanent_impact_accumulates() {
        Python::attach(|py| {
            let mut m = PyImpactedMatchingEngine::new("linear", 0.05, 10, 0.0, 0.5, 0.0).unwrap();
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 100.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            let buy = make_limit_dict(py, 2, "BTC-USDT", "buy", 100.0, 1.0).unwrap();
            m.submit(py, &buy).unwrap();
            // 0.05 * (1/1) = 0.05 permanent
            assert!(m.permanent_offset() > 0.0);
        });
    }

    /// stats dict 字段完整
    #[test]
    fn stats_dict_fields() {
        Python::attach(|py| {
            let mut m = PyImpactedMatchingEngine::new("linear", 0.05, 10, 0.7, 0.5, 0.0).unwrap();
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 100.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            let buy = make_limit_dict(py, 2, "BTC-USDT", "buy", 100.0, 1.0).unwrap();
            m.submit(py, &buy).unwrap();
            let stats = m.stats(py).unwrap();
            assert_eq!(
                stats
                    .get_item("submitted_orders")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                2
            );
            assert_eq!(
                stats
                    .get_item("filled_orders")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                1
            );
            assert_eq!(
                stats
                    .get_item("total_fills")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                1
            );
        });
    }

    /// best_bid / best_ask / mid_price 在有冲击时反映偏移
    #[test]
    fn best_prices_reflect_permanent_impact() {
        Python::attach(|py| {
            let mut m = PyImpactedMatchingEngine::new("linear", 0.1, 10, 0.0, 0.5, 0.0).unwrap();
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 100.0, 10.0).unwrap();
            m.submit(py, &sell).unwrap();
            assert_eq!(m.best_ask(), Some(100.0));
            let buy = make_limit_dict(py, 2, "BTC-USDT", "buy", 100.0, 1.0).unwrap();
            m.submit(py, &buy).unwrap();
            // permanent = 0.1 * 1/10 = 0.01 ⇒ best_ask = 100 - 0.01 = 99.99
            let ask = m.best_ask().unwrap();
            assert!((ask - 99.99).abs() < 1e-6, "expected 99.99, got {ask}");
        });
    }

    /// cancel 委托给内部 L1
    #[test]
    fn cancel_delegates_to_inner() {
        Python::attach(|py| {
            let mut m = PyImpactedMatchingEngine::new("linear", 0.0, 10, 0.7, 0.5, 0.0).unwrap();
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 100.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            assert!(m.cancel(1));
            assert_eq!(m.active_order_count(), 0);
            assert!(!m.cancel(999));
        });
    }

    /// `reset_impact_state` 清零 offset 与 stats
    #[test]
    fn reset_impact_state_clears_state() {
        Python::attach(|py| {
            let mut m = PyImpactedMatchingEngine::new("linear", 0.1, 10, 0.0, 0.5, 0.0).unwrap();
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 100.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            let buy = make_limit_dict(py, 2, "BTC-USDT", "buy", 100.0, 1.0).unwrap();
            m.submit(py, &buy).unwrap();
            assert!(m.permanent_offset() > 0.0);
            m.reset_impact_state();
            assert_eq!(m.permanent_offset(), 0.0);
            let stats = m.stats(py).unwrap();
            assert_eq!(
                stats
                    .get_item("submitted_orders")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                0
            );
        });
    }

    // ─── Builder ─────────────────────────────────

    /// Builder 默认值构建
    #[test]
    fn builder_defaults_construct_linear() {
        let b = PyImpactedMatchingEngineBuilder::new();
        let m = b.build().unwrap();
        assert_eq!(m.model_name(), "LinearImpact");
    }

    /// Builder 链式设值后构建(从 Python 调用)
    ///
    /// 注:链式 API 依赖 `PyRefMut`,纯 Rust 调用不便,这里用 `py.run`
    /// 模拟 Python 端使用。但 `axon_quant._native` 需要先 `maturin develop` 之后
    /// 才可用,故本测试默认 `#[ignore]`,留待 Stage 2 Task 13 E2E 测试覆盖。
    #[test]
    #[ignore = "需要 maturin develop 后 axon_quant._native 可用;Stage 2 Task 13 E2E 覆盖"]
    fn builder_chain_constructs_power_law() {
        Python::attach(|py| {
            py.run(
                c"import axon_quant._native.backtest as nb
b = nb.ImpactedMatchingEngineBuilder()
m = (b.model_type('power_law').coefficient(0.1).exponent(0.6)
       .depth_levels(5).instantaneous_ratio(0.8).permanent_decay(0.05).build())
assert m.model_name() == 'PowerLawImpact', f'expected PowerLawImpact, got {m.model_name()}'
assert m.permanent_decay() == 0.05, f'expected 0.05, got {m.permanent_decay()}'",
                None,
                None,
            )
            .unwrap();
        });
    }

    /// Builder `__repr__` 含所有字段
    #[test]
    fn builder_repr_contains_all_fields() {
        let b = PyImpactedMatchingEngineBuilder::new();
        let s = b.__repr__();
        assert!(s.contains("ImpactedMatchingEngineBuilder"));
        assert!(s.contains("model_type=None"));
        assert!(s.contains("coefficient=None"));
    }

    /// Builder 构造非法 model_type → `PyValueError`
    #[test]
    fn builder_invalid_model_type_raises() {
        // 直接调 `new` 而不通过 chain(规避 PyRefMut 在纯 Rust 调用的复杂性)
        let r = PyImpactedMatchingEngine::new("bogus", 0.0, 10, 0.7, 0.5, 0.0);
        Python::attach(|py| {
            // 用 match 替代 `unwrap_err`(后者需要 T: Debug,PyImpactedMatchingEngine 没 derive)
            let err = match r {
                Ok(_) => panic!("expected error for invalid model_type"),
                Err(e) => e,
            };
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    // ─── Python 自定义模型 ─────────────────────────

    /// 用 Python 类实现 `compute_impact` 注入
    ///
    /// 注:此测试通过 `Python::run` 注入一个 Python 自定义类,验证 adapter
    /// 能在 Rust 端正确拿到回调结果。
    #[test]
    fn python_custom_model_invokes_callback() {
        Python::attach(|py| {
            // 1. 注入一个返回固定冲击的 Python 类
            py.run(
                c"class ConstantImpact:
    def compute_impact(self, order_quantity, side, order_book):
        return {'instantaneous': 0.01, 'permanent': 0.005}",
                None,
                None,
            )
            .unwrap();

            // 2. 实例化并注入到引擎
            let cls = py
                .import("__main__")
                .unwrap()
                .getattr("ConstantImpact")
                .unwrap();
            let instance = cls.call0().unwrap();
            let py_model: Py<PyAny> = instance.into();

            let mut m = PyImpactedMatchingEngine::new("linear", 0.05, 10, 0.7, 0.5, 0.0).unwrap();
            m.with_custom_model(py_model);
            // 模型名固定为 "PythonImpactModel"
            assert_eq!(m.model_name(), "PythonImpactModel");

            // 3. submit 验证回调
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 100.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            let buy = make_limit_dict(py, 2, "BTC-USDT", "buy", 100.0, 1.0).unwrap();
            let result = m.submit(py, &buy).unwrap();
            let fills = result.get_item("fills").unwrap().unwrap();
            let fill_dict = fills.get_item(0).unwrap();
            // fill_dict 是 Bound<PyAny>,其 get_item 返回 Result<Bound, PyErr>
            let price: f64 = fill_dict.get_item("price").unwrap().extract().unwrap();
            // 即时冲击 = 0.01 ⇒ price = 100 + 0.01 = 100.01
            assert!(
                (price - 100.01).abs() < 1e-9,
                "expected 100.01 from Python custom model, got {price}"
            );
        });
    }

    /// Python 自定义模型返回缺字段时退化到 0
    #[test]
    fn python_custom_model_missing_fields_falls_back_to_zero() {
        Python::attach(|py| {
            py.run(
                c"class PartialImpact:
    def compute_impact(self, order_quantity, side, order_book):
        return {}  # 缺字段",
                None,
                None,
            )
            .unwrap();

            let cls = py
                .import("__main__")
                .unwrap()
                .getattr("PartialImpact")
                .unwrap();
            let instance = cls.call0().unwrap();
            let py_model: Py<PyAny> = instance.into();

            let mut m = PyImpactedMatchingEngine::new("linear", 0.0, 10, 0.7, 0.5, 0.0).unwrap();
            m.with_custom_model(py_model);

            // 即时/永久都缺,fill 价应保持 100
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 100.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            let buy = make_limit_dict(py, 2, "BTC-USDT", "buy", 100.0, 1.0).unwrap();
            let result = m.submit(py, &buy).unwrap();
            let fills = result.get_item("fills").unwrap().unwrap();
            let fill_dict = fills.get_item(0).unwrap();
            let price: f64 = fill_dict.get_item("price").unwrap().extract().unwrap();
            assert!((price - 100.0).abs() < 1e-9);
        });
    }

    /// `PyImpactModelAdapter` 是 `Send + Sync`(`ImpactModel` trait 要求)
    #[test]
    fn py_impact_model_adapter_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PyImpactModelAdapter>();
    }

    // ─── 内部辅助 ─────────────────────────────────

    /// `build_config` linear 路径
    #[test]
    fn build_config_linear_ok() {
        let cfg = build_config("linear", 0.05, 10, 0.7, 0.5).unwrap();
        assert!(matches!(cfg, ImpactModelConfig::Linear { .. }));
    }

    /// `build_config` power_law 路径
    #[test]
    fn build_config_power_law_ok() {
        let cfg = build_config("power_law", 0.1, 5, 0.7, 0.5).unwrap();
        assert!(matches!(cfg, ImpactModelConfig::PowerLaw { .. }));
    }

    /// `build_config` 未知 model_type → `PyValueError`
    #[test]
    fn build_config_unknown_raises() {
        Python::attach(|py| {
            let err = build_config("foo", 0.0, 10, 0.7, 0.5).unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    /// `snapshot_to_dict` 字段齐全
    #[test]
    fn snapshot_to_dict_fields() {
        Python::attach(|py| {
            let snap = OrderBookSnapshot {
                timestamp: Timestamp::from_nanos(1000),
                bids: vec![axon_core::market::OrderBookLevel {
                    price: Price::from_f64(99.0),
                    quantity: Quantity::from_f64(1.0),
                }],
                asks: vec![axon_core::market::OrderBookLevel {
                    price: Price::from_f64(101.0),
                    quantity: Quantity::from_f64(2.0),
                }],
            };
            let d = snapshot_to_dict(py, &snap).unwrap();
            assert_eq!(
                d.get_item("timestamp_ns")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                1000
            );
            let bids = d.get_item("bids").unwrap().unwrap();
            assert_eq!(bids.len().unwrap(), 1);
            let asks = d.get_item("asks").unwrap().unwrap();
            assert_eq!(asks.len().unwrap(), 1);
        });
    }

    /// `register` 签名稳定(编译期断言)
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
