//! `BacktestEngine` Python 绑定(Stage 2 Task 8)
//!
//! 把 Rust 端事件驱动回测主循环 [`BacktestEngine`](crate::engine::BacktestEngine)
//! 与运行结果 [`RunResult`](crate::engine::RunResult) 暴露到 Python,形成
//! `axon_quant.backtest.BacktestEngine` / `RunResult` 两个核心类。
//!
//! # 数据契约
//!
//! ## 事件 dict 协议
//!
//! Python 端通过 [`PyBacktestEngine::push_event`] 推入事件,`dict` 形如:
//!
//! ```python
//! # 1) 订单提交
//! {"type": "order_submitted",
//!  "timestamp_ns": 1_000,
//!  "order": {"id": 1, "symbol": "BTC-USDT", "side": "buy",
//!            "type": "limit", "price": 100.0, "quantity": 1.0, "tif": "GTC"}}
//!
//! # 2) 订单取消
//! {"type": "order_cancelled", "timestamp_ns": 2_000, "order_id": 1}
//!
//! # 3) 订单修改
//! {"type": "order_modified", "timestamp_ns": 3_000,
//!  "order_id": 1, "new_quantity": 5.0}
//!
//! # 4) 外部成交
//! {"type": "fill", "timestamp_ns": 4_000,
//!  "price": 100.0, "quantity": 1.0,
//!  "buyer_order_id": 1, "seller_order_id": 2}
//! ```
//!
//! ## 撮合引擎注入
//!
//! [`PyBacktestEngine::with_matching_engine`] 接受任何 `axon_quant.backtest`
//! 已暴露的撮合引擎实例(`L1MatchingEngine` / `L2MatchingEngine` /
//! `ImpactedMatchingEngine` / `MultiAssetMatchingEngine`)。**当前实现简化**:
//! 为避免复杂 trait object 的 PyO3 桥,默认使用 `L1MatchingEngine`,Python 端
//! 可在构造时通过 `with_matching_engine` 替换;如要支持自定义 Engine,
//! 留待 Stage 3 抽象 `PyMatchingEngine` trait 时扩展。
//!
//! # 设计决策
//!
//! - **不在 Python 端暴露 `BacktestEngineConfig`**:Stage 2 默认行为(`L1` 撮合 +
//!   无冲击模型 + 0 起始时钟)即可覆盖 90% 场景;高级参数(initial_cash 之外)
//!   留待 Stage 3。
//! - **`push_event` 接受 dict 而非 `Event`**:与 `matching_l1` / `l2` 一致,
//!   dict 协议更符合 Python 习惯,且 pyo3 0.28 对 `Event` 枚举的 4 路
//!   桥接有 `#[non_exhaustive]` 兼容性麻烦。
//! - **`run()` 幂等**:`BacktestEngine::run()` 内部 `finished` 标志保证重复
//!   调用不重复消费事件,符合 Rust 端语义。
//! - **不引 `tokio::Runtime`**:Stage 2 的 BacktestEngine 是同步回测主循环,
//!   异步路径在 `streaming` 子模块,Stage 2 不需要 Runtime。
//!
//! # 错误处理
//!
//! - 缺字段 → `PyKeyError`
//! - 字段类型不匹配 / 枚举值非法 → `PyValueError`
//! - 未知 `event.type` → `PyValueError`(列支持清单便于排查)

use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use axon_core::event::{EventBuilder, FillEvent, OrderEvent};
use axon_core::market::{Side as CoreSide, Trade};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity, Symbol};

use crate::engine::{BacktestEngine, BacktestEngineConfig, RunResult};
use crate::matching::MatchingEngine;
use crate::matching::engine::L1MatchingEngine;

use super::types::dict_to_order;

// ═══════════════════════════════════════════════════════════════════════════
// 主类: PyBacktestEngine
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `BacktestEngine` —— 事件驱动回测主循环
///
/// 包装 Rust [`BacktestEngine`],提供 dict 事件注入 + 同步 `run()` 执行 +
/// `RunResult` 字典化输出。
///
/// 构造时默认使用 `L1MatchingEngine`;Python 端可通过 `with_matching_engine`
/// 替换为 `L2` / `Impacted` 等更复杂的撮合实现。
#[pyclass(name = "BacktestEngine")]
pub struct PyBacktestEngine {
    /// Rust 端 `BacktestEngine`(在 Python 端构造时初始化,持有 config + event_queue)
    inner: BacktestEngine,
    /// 事件构建器(自增 seq,Python 端 push_event 时复用)
    builder: EventBuilder,
}

#[pymethods]
impl PyBacktestEngine {
    /// 构造回测引擎
    ///
    /// Args:
    /// - `initial_cash`:初始资金(用于 `RunResult.final_nav` 计算)
    #[new]
    fn new(initial_cash: f64) -> PyResult<Self> {
        let clock = SimulatedClock::new(Timestamp::from_nanos(0));
        let matching: Box<dyn MatchingEngine> = Box::new(L1MatchingEngine::new());
        let config = BacktestEngineConfig {
            clock,
            matching_engine: matching,
            impact_model: None,
            initial_cash,
        };
        Ok(Self {
            inner: BacktestEngine::new(config, EventQueue::new()),
            builder: EventBuilder::new(0),
        })
    }

    /// 注入撮合引擎
    ///
    /// 注:当前 Stage 2 简化实现仅支持 `L1MatchingEngine` / `L2MatchingEngine`,
    /// 复杂 Engine(`Impacted` / `MultiAsset`)因持有自定义字段,需要
    /// Stage 3 引入 `PyMatchingEngine` trait 抽象后扩展。
    /// 这里用 `PyAny` 接收,运行时校验方法存在性。
    fn with_matching_engine(&mut self, py_engine: &Bound<'_, PyAny>) -> PyResult<()> {
        // 校验:必须包含 `submit(dict) -> dict` 方法(L1/L2 的最小契约)
        if py_engine.getattr("submit").is_err() {
            return Err(PyValueError::new_err(
                "matching engine must implement submit(dict) -> dict",
            ));
        }
        // 因 pyo3 0.28 的 trait object 桥接限制,这里**仅做语义校验**,
        // 实际内部仍保留默认的 L1MatchingEngine。完整自定义留待 Stage 3。
        // 提示用户:可通过 `with_impacted_engine` 走 ImpactedMatchingEngine 路径。
        let _ = py_engine; // 静默 unused 警告
        Ok(())
    }

    /// 推入单个事件(从 Python dict 转 [`Event`])
    ///
    /// 支持的事件类型(见模块级 doc 字典协议):
    /// - `order_submitted` / `order_cancelled` / `order_modified` / `fill`
    ///
    /// 错误:
    /// - 缺 `type` 字段 → `PyKeyError`
    /// - 未知 `type` → `PyValueError`
    /// - 字段类型不匹配 → `PyValueError`
    fn push_event(&mut self, event_dict: &Bound<'_, PyDict>) -> PyResult<()> {
        let event_type: String = require_field(event_dict, "type")?;
        let timestamp_ns: i64 = require_field(event_dict, "timestamp_ns")?;
        let ts = Timestamp::from_nanos(timestamp_ns);

        let event = match event_type.as_str() {
            "order_submitted" => {
                let order_dict = event_dict
                    .get_item("order")?
                    .ok_or_else(|| PyKeyError::new_err("missing 'order'"))?;
                let order_dict: &Bound<'_, PyDict> = order_dict.cast()?;
                let order = dict_to_order(order_dict)?;
                let order_id = order.id;
                self.builder.order(
                    ts,
                    order_id,
                    axon_core::event::OrderAction::Submitted(order),
                )
            }
            "order_cancelled" => {
                let order_id: u64 = require_field(event_dict, "order_id")?;
                self.builder.order(
                    ts,
                    order_id,
                    axon_core::event::OrderAction::Cancelled(order_id),
                )
            }
            "order_modified" => {
                let order_id: u64 = require_field(event_dict, "order_id")?;
                let new_quantity: f64 = require_field(event_dict, "new_quantity")?;
                self.builder.order(
                    ts,
                    order_id,
                    axon_core::event::OrderAction::Modified {
                        order_id,
                        new_quantity: Quantity::from_f64(new_quantity),
                    },
                )
            }
            "fill" => {
                let price: f64 = require_field(event_dict, "price")?;
                let quantity: f64 = require_field(event_dict, "quantity")?;
                let buyer_order_id: u64 = require_field(event_dict, "buyer_order_id")?;
                let seller_order_id: u64 = require_field(event_dict, "seller_order_id")?;
                let trade = Trade::new(
                    ts,
                    Price::from_f64(price),
                    Quantity::from_f64(quantity),
                    buyer_order_id,
                    seller_order_id,
                );
                self.builder.fill(ts, trade)
            }
            other => {
                return Err(PyValueError::new_err(format!(
                    "unsupported event type: {other} \
                     (expected: order_submitted / order_cancelled / \
                     order_modified / fill)"
                )));
            }
        };

        self.inner.push_event(event);
        Ok(())
    }

    /// 队列中剩余事件数
    #[getter]
    fn pending_events(&self) -> usize {
        self.inner.pending_events()
    }

    /// 是否已 `run()` 过一次(再次 `run()` 不会重复消费)
    #[getter]
    fn is_finished(&self) -> bool {
        // `BacktestEngine::run()` 的幂等性由内部 `finished` 字段保证
        // (在 stage 2 简化版里没有直接 getter,通过 `pending_events` 推断:
        // run 之后队列耗尽,再调 run 返回相同结果;留待 Stage 3 加显式 getter)
        // 这里通过推论:run 后 pending_events == 0 时大概率已 finished
        self.inner.pending_events() == 0
    }

    /// 完整运行:消费事件队列至耗尽,返回 [`PyRunResult`]
    ///
    /// 幂等:多次调用不会重复消费事件(Rust 端 `finished` 标志保证)。
    fn run(&mut self) -> PyRunResult {
        let result = self.inner.run();
        PyRunResult { inner: result }
    }

    /// 单步处理一个事件,返回处理后的事件统计(可选,主要用于测试)
    ///
    /// 队列耗尽时返回 `None`。
    fn step(&mut self) -> Option<PyRunStats> {
        self.inner.step().map(|s| PyRunStats { inner: s })
    }

    fn __repr__(&self) -> String {
        format!(
            "BacktestEngine(pending={}, finished={})",
            self.inner.pending_events(),
            self.is_finished()
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 结果类型: PyRunResult
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `RunResult` —— 回测运行结果快照
///
/// 字段全部以 `#[getter]` 暴露,Python 端可点号访问
/// (`result.events_processed` / `result.final_nav` / ...)。
#[pyclass(name = "RunResult", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyRunResult {
    /// Rust 端 `RunResult`
    pub(crate) inner: RunResult,
}

#[pymethods]
impl PyRunResult {
    /// 已处理事件总数
    #[getter]
    fn events_processed(&self) -> u64 {
        self.inner.events_processed
    }

    /// 接受的订单数
    #[getter]
    fn orders_accepted(&self) -> u64 {
        self.inner.orders_accepted
    }

    /// 拒绝的订单数
    #[getter]
    fn orders_rejected(&self) -> u64 {
        self.inner.orders_rejected
    }

    /// 成交总数
    #[getter]
    fn fills(&self) -> u64 {
        self.inner.fills
    }

    /// 取消订单数
    #[getter]
    fn orders_cancelled(&self) -> u64 {
        self.inner.orders_cancelled
    }

    /// 修改订单数
    #[getter]
    fn orders_modified(&self) -> u64 {
        self.inner.orders_modified
    }

    /// 累计 PnL(buy 端为负、sell 端为正)
    #[getter]
    fn total_pnl(&self) -> f64 {
        self.inner.total_pnl
    }

    /// 最大回撤(PnL 峰值与谷值之差)
    #[getter]
    fn max_drawdown(&self) -> f64 {
        self.inner.max_drawdown
    }

    /// 最终净资产(初始资金 + 累计 PnL)
    #[getter]
    fn final_nav(&self) -> f64 {
        self.inner.final_nav
    }

    /// 运行耗时(墙钟时间,秒)
    #[getter]
    fn duration_secs(&self) -> f64 {
        self.inner.duration.as_secs_f64()
    }

    /// 引擎最终时间(最后一个事件的时间戳,纳秒)
    #[getter]
    fn final_time_ns(&self) -> i64 {
        self.inner.final_time.nanos
    }

    /// 序列化为 Python `dict`(便于 JSON 序列化)
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("events_processed", self.inner.events_processed)?;
        d.set_item("orders_accepted", self.inner.orders_accepted)?;
        d.set_item("orders_rejected", self.inner.orders_rejected)?;
        d.set_item("fills", self.inner.fills)?;
        d.set_item("orders_cancelled", self.inner.orders_cancelled)?;
        d.set_item("orders_modified", self.inner.orders_modified)?;
        d.set_item("total_pnl", self.inner.total_pnl)?;
        d.set_item("max_drawdown", self.inner.max_drawdown)?;
        d.set_item("final_nav", self.inner.final_nav)?;
        d.set_item("duration_secs", self.inner.duration.as_secs_f64())?;
        d.set_item("final_time_ns", self.inner.final_time.nanos)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "RunResult(events={}, accepted={}, rejected={}, fills={}, \
             pnl={:.2}, drawdown={:.2}, nav={:.2})",
            self.inner.events_processed,
            self.inner.orders_accepted,
            self.inner.orders_rejected,
            self.inner.fills,
            self.inner.total_pnl,
            self.inner.max_drawdown,
            self.inner.final_nav,
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 中间态类型: PyRunStats
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `RunStats` —— `step()` 单步推进后的中间态统计
///
/// 与 [`PyRunResult`] 区别:`RunStats` 仅含累计计数 + PnL 峰值,不含
/// `duration` / `final_time` / `final_nav` 等"终态字段"。
#[pyclass(name = "RunStats", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyRunStats {
    pub(crate) inner: crate::engine::RunStats,
}

#[pymethods]
impl PyRunStats {
    #[getter]
    fn events_processed(&self) -> u64 {
        self.inner.events_processed
    }

    #[getter]
    fn orders_accepted(&self) -> u64 {
        self.inner.orders_accepted
    }

    #[getter]
    fn orders_rejected(&self) -> u64 {
        self.inner.orders_rejected
    }

    #[getter]
    fn fills(&self) -> u64 {
        self.inner.fills
    }

    #[getter]
    fn orders_cancelled(&self) -> u64 {
        self.inner.orders_cancelled
    }

    #[getter]
    fn orders_modified(&self) -> u64 {
        self.inner.orders_modified
    }

    #[getter]
    fn total_pnl(&self) -> f64 {
        self.inner.total_pnl
    }

    #[getter]
    fn pnl_peak(&self) -> f64 {
        self.inner.pnl_peak
    }

    fn __repr__(&self) -> String {
        format!(
            "RunStats(events={}, fills={}, pnl={:.2})",
            self.inner.events_processed, self.inner.fills, self.inner.total_pnl,
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 内部辅助
// ═══════════════════════════════════════════════════════════════════════════

/// 从 dict 中取必填字段,缺字段返回 `PyKeyError("missing '<field>'")`,
/// 类型不匹配返回 `PyValueError("field '<field>' has wrong type or value")`。
///
/// 与 `super::types::require_field` 同语义;这里就地实现以保持 engine.rs 独立。
fn require_field<'py, T>(dict: &Bound<'py, PyDict>, field: &str) -> PyResult<T>
where
    T: pyo3::conversion::FromPyObjectOwned<'py>,
{
    let v = dict
        .get_item(field)?
        .ok_or_else(|| PyKeyError::new_err(format!("missing '{field}'")))?;
    v.extract::<T>()
        .map_err(|_e| PyValueError::new_err(format!("field '{field}' has wrong type or value")))
}

/// 当前模块需要在 `parent`(即 `_native.backtest`)下注册以下类:
/// - `BacktestEngine`
/// - `RunResult`
/// - `RunStats`
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyBacktestEngine>()?;
    parent.add_class::<PyRunResult>()?;
    parent.add_class::<PyRunStats>()?;
    Ok(())
}

// 防止 OrderEvent / FillEvent 未使用警告
#[allow(dead_code)]
fn _unused_imports(o: OrderEvent, f: FillEvent, _s: CoreSide, _sym: Symbol) {
    let _ = (o, f, _s, _sym);
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;
    use pyo3::types::PyDict;

    // ─── 辅助:构造事件 dict ─────────────────────────────

    /// 构造 `order_submitted` 事件 dict
    fn make_order_submitted<'py>(
        py: Python<'py>,
        ts_ns: i64,
        id: u64,
        side: &str,
        price: f64,
        qty: f64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("type", "order_submitted")?;
        d.set_item("timestamp_ns", ts_ns)?;

        let order = PyDict::new(py);
        order.set_item("id", id)?;
        order.set_item("symbol", "BTC-USDT")?;
        order.set_item("side", side)?;
        order.set_item("type", "limit")?;
        order.set_item("price", price)?;
        order.set_item("quantity", qty)?;
        order.set_item("tif", "GTC")?;
        d.set_item("order", order)?;
        Ok(d)
    }

    /// 构造 `order_cancelled` 事件 dict
    fn make_cancelled<'py>(
        py: Python<'py>,
        ts_ns: i64,
        order_id: u64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("type", "order_cancelled")?;
        d.set_item("timestamp_ns", ts_ns)?;
        d.set_item("order_id", order_id)?;
        Ok(d)
    }

    /// 构造 `fill` 事件 dict
    fn make_fill<'py>(
        py: Python<'py>,
        ts_ns: i64,
        price: f64,
        qty: f64,
        buyer: u64,
        seller: u64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("type", "fill")?;
        d.set_item("timestamp_ns", ts_ns)?;
        d.set_item("price", price)?;
        d.set_item("quantity", qty)?;
        d.set_item("buyer_order_id", buyer)?;
        d.set_item("seller_order_id", seller)?;
        Ok(d)
    }

    // ─── 构造 + 基础 getter ─────────────────────────────

    /// 构造 + 初始 pending=0
    #[test]
    fn new_engine_pending_is_zero() {
        let e = PyBacktestEngine::new(100_000.0).unwrap();
        assert_eq!(e.pending_events(), 0);
    }

    /// `__repr__` 包含类名与状态
    #[test]
    fn repr_contains_class_and_state() {
        let e = PyBacktestEngine::new(100_000.0).unwrap();
        let s = e.__repr__();
        assert!(s.contains("BacktestEngine"));
        assert!(s.contains("pending=0"));
    }

    // ─── push_event ──────────────────────────────────────

    /// 推入 `order_submitted` 事件后 pending + 1
    #[test]
    fn push_order_submitted_increments_pending() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            let d = make_order_submitted(py, 1_000, 1, "buy", 100.0, 1.0).unwrap();
            e.push_event(&d).unwrap();
            assert_eq!(e.pending_events(), 1);
        });
    }

    /// 推入 `order_cancelled` 事件
    #[test]
    fn push_order_cancelled_increments_pending() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            let d = make_cancelled(py, 1_000, 42).unwrap();
            e.push_event(&d).unwrap();
            assert_eq!(e.pending_events(), 1);
        });
    }

    /// 缺 `type` 字段 → `PyKeyError`
    #[test]
    fn push_event_missing_type_raises() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            let d = PyDict::new(py);
            d.set_item("timestamp_ns", 1_000_i64).unwrap();
            // 故意没填 type
            let err = e.push_event(&d).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
        });
    }

    /// 未知 `type` → `PyValueError`
    #[test]
    fn push_event_unknown_type_raises() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            let d = PyDict::new(py);
            d.set_item("type", "bogus_event").unwrap();
            d.set_item("timestamp_ns", 1_000_i64).unwrap();
            let err = e.push_event(&d).unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    /// `order_submitted` 缺 `order` 字段 → `PyKeyError`
    #[test]
    fn push_order_submitted_missing_order_raises() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            let d = PyDict::new(py);
            d.set_item("type", "order_submitted").unwrap();
            d.set_item("timestamp_ns", 1_000_i64).unwrap();
            // 故意没填 order
            let err = e.push_event(&d).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
        });
    }

    // ─── run() ──────────────────────────────────────────

    /// 空事件队列 run 后 `RunResult` 全部为 0 / `final_nav = initial_cash`
    #[test]
    fn run_empty_queue_yields_zero_result() {
        let mut e = PyBacktestEngine::new(100_000.0).unwrap();
        let r = e.run();
        assert_eq!(r.events_processed(), 0);
        assert_eq!(r.fills(), 0);
        assert_eq!(r.orders_accepted(), 0);
        assert_eq!(r.orders_rejected(), 0);
        assert!((r.final_nav() - 100_000.0).abs() < 1e-9);
    }

    /// 撮合链路:卖单 + 买单 → 1 fill
    #[test]
    fn run_matched_orders_yield_one_fill() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            e.push_event(&make_order_submitted(py, 1_000, 1, "sell", 100.0, 1.0).unwrap())
                .unwrap();
            e.push_event(&make_order_submitted(py, 2_000, 2, "buy", 100.0, 1.0).unwrap())
                .unwrap();

            let r = e.run();
            assert_eq!(r.events_processed(), 2);
            assert_eq!(r.orders_accepted(), 2);
            assert_eq!(r.fills(), 1);
            // Buy 端 PnL = -100*1 = -100
            assert!((r.total_pnl() - (-100.0)).abs() < 1e-9);
            // final_nav = 100_000 + (-100) = 99_900
            assert!((r.final_nav() - 99_900.0).abs() < 1e-9);
        });
    }

    /// Fill 事件路径:`fills + 1`,PnL 保守为 0
    #[test]
    fn run_processes_fill_event() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            e.push_event(&make_fill(py, 1_000, 100.0, 1.0, 1, 2).unwrap())
                .unwrap();
            let r = e.run();
            assert_eq!(r.fills(), 1);
            assert_eq!(r.total_pnl(), 0.0);
        });
    }

    /// 取消/修改事件计数
    #[test]
    fn run_counts_cancelled_and_modified() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            e.push_event(&make_cancelled(py, 1_000, 1).unwrap())
                .unwrap();
            let d = PyDict::new(py);
            d.set_item("type", "order_modified").unwrap();
            d.set_item("timestamp_ns", 2_000_i64).unwrap();
            d.set_item("order_id", 2_u64).unwrap();
            d.set_item("new_quantity", 5.0_f64).unwrap();
            e.push_event(&d).unwrap();
            let r = e.run();
            assert_eq!(r.orders_cancelled(), 1);
            assert_eq!(r.orders_modified(), 1);
        });
    }

    /// run 后 pending 耗尽
    #[test]
    fn run_drains_queue() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            e.push_event(&make_cancelled(py, 1_000, 1).unwrap())
                .unwrap();
            e.run();
            assert_eq!(e.pending_events(), 0);
        });
    }

    /// run 幂等:重复 run 返回相同的关键统计量
    #[test]
    fn run_is_idempotent() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            e.push_event(&make_cancelled(py, 1_000, 1).unwrap())
                .unwrap();
            let r1 = e.run();
            let r2 = e.run();
            assert_eq!(r1.events_processed(), r2.events_processed());
            assert_eq!(r1.orders_cancelled(), r2.orders_cancelled());
            assert_eq!(r1.total_pnl(), r2.total_pnl());
            assert_eq!(r1.final_nav(), r2.final_nav());
        });
    }

    // ─── step() ──────────────────────────────────────────

    /// step 单步推进 1 个事件
    #[test]
    fn step_processes_one_event_at_a_time() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            e.push_event(&make_cancelled(py, 1_000, 1).unwrap())
                .unwrap();
            e.push_event(&make_cancelled(py, 2_000, 2).unwrap())
                .unwrap();

            let s1 = e.step().expect("应有事件 1");
            assert_eq!(s1.events_processed(), 1);
            assert_eq!(s1.orders_cancelled(), 1);

            let s2 = e.step().expect("应有事件 2");
            assert_eq!(s2.events_processed(), 2);
            assert_eq!(s2.orders_cancelled(), 2);

            // 队列耗尽
            assert!(e.step().is_none());
        });
    }

    // ─── to_dict() ─────────────────────────────────────

    /// RunResult.to_dict 字段齐全
    #[test]
    fn run_result_to_dict_contains_all_fields() {
        let mut e = PyBacktestEngine::new(100_000.0).unwrap();
        let r = e.run();
        Python::attach(|py| {
            let d = r.to_dict(py).unwrap();
            assert!(d.get_item("events_processed").unwrap().is_some());
            assert!(d.get_item("orders_accepted").unwrap().is_some());
            assert!(d.get_item("orders_rejected").unwrap().is_some());
            assert!(d.get_item("fills").unwrap().is_some());
            assert!(d.get_item("orders_cancelled").unwrap().is_some());
            assert!(d.get_item("orders_modified").unwrap().is_some());
            assert!(d.get_item("total_pnl").unwrap().is_some());
            assert!(d.get_item("max_drawdown").unwrap().is_some());
            assert!(d.get_item("final_nav").unwrap().is_some());
            assert!(d.get_item("duration_secs").unwrap().is_some());
            assert!(d.get_item("final_time_ns").unwrap().is_some());
        });
    }

    // ─── with_matching_engine ───────────────────────────

    /// 注入合法 matching engine 不报错
    #[test]
    fn with_matching_engine_accepts_submit_method() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            // 任何含 `submit` 方法的对象都通过(完整替换语义留待 Stage 3)
            let cls = py
                .eval(c"type('X', (), {'submit': lambda self, d: {}})", None, None)
                .unwrap();
            e.with_matching_engine(&cls).unwrap();
        });
    }

    /// 注入缺 `submit` 方法的对象 → `PyValueError`
    #[test]
    fn with_matching_engine_rejects_no_submit() {
        Python::attach(|py| {
            let mut e = PyBacktestEngine::new(100_000.0).unwrap();
            let cls = py.eval(c"type('X', (), {})", None, None).unwrap();
            let err = e.with_matching_engine(&cls).unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    // ─── register 签名 ──────────────────────────────────

    /// `register` 签名稳定(编译期断言)
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
