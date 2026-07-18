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
use pyo3::types::{PyDict, PyList, PyTuple};

use axon_core::event::{EventBuilder, FillEvent, FundingSchedule, MarkEvent, OrderEvent};
use axon_core::market::{Side as CoreSide, Trade};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Price, Quantity, Symbol};

use crate::engine::{BacktestEngine, BacktestEngineConfig, RunResult};
use crate::matching::MatchingEngine;
use crate::matching::engine::L1MatchingEngine;

use super::types::dict_to_order;
use crate::python::matching::PyMatchingEngine;

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
            fee_config: crate::engine::FeeConfig::default(),
            // Stage 3 阶段 C(强制平仓):默认关闭,需 Python 端显式调
            // `with_force_liquidate(True)` 启用,避免误触发末日单污染 PnL
            force_liquidate: false,
        };
        Ok(Self {
            inner: BacktestEngine::new(config, EventQueue::new()),
            builder: EventBuilder::new(0),
        })
    }

    /// 注入撮合引擎(Stage 3:真替换,不再仅校验方法存在性)
    ///
    /// 接受任何含 `submit(dict) -> dict` 方法的 Python 对象(包括
    /// `L1MatchingEngine` / `L2MatchingEngine` / `ImpactedMatchingEngine` /
    /// 用户自定义的撮合引擎类)。通过 `PyMatchingEngine` 桥接成
    /// Rust `MatchingEngine` trait object,**真替换**引擎内部默认的
    /// `L1MatchingEngine`。
    fn with_matching_engine(&mut self, py_engine: &Bound<'_, PyAny>) -> PyResult<()> {
        let py_matcher = PyMatchingEngine::new(py_engine)?;
        self.inner.replace_matching_engine(Box::new(py_matcher));
        Ok(())
    }

    /// 注入手续费配置(Stage 3 阶段 B 任务 B4)
    ///
    /// Args:
    /// - `taker_rate`: Taker 手续费率(0.001 = 0.1%)。`notional * taker_rate` 按每笔 fill 累计。
    ///
    /// 不传任何参数时使用 `FeeConfig::default()`(0.1%)。
    ///
    /// Returns:
    /// - `&mut Self` 供链式调用(如 `engine.with_fee_config(0.001).with_force_liquidate(True)`)
    fn with_fee_config<'py>(
        mut slf: PyRefMut<'py, Self>,
        taker_rate: f64,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.inner.with_fee_config(taker_rate);
        Ok(slf)
    }

    /// EOD 强制平仓开关(Stage 3 阶段 C 新增)
    ///
    /// - `on=False` (默认):保留策略意图,`final_nav` 用 `equity_curve` 末帧 mark 估值
    ///   (回测更"忠实"于策略信号,但未平仓单子的浮盈浮亏会计入 total_pnl)
    /// - `on=True`:回测结束时遍历 `position_states`,对每个非零持仓发市价单
    ///   (IOC,撮合引擎当前最优对手价),把剩余持仓全部清仓后才算终态。
    ///   适合需要"每日盈亏都转为已实现"的对账/报表场景
    ///   (注意:末日单若 PnL 不理想会污染结果,业内通常在回测脚本里手动控制开关)
    ///
    /// Args:
    /// - `on`:True 启用强制平仓,False 关闭
    ///
    /// Returns:
    /// - `&mut Self` 供链式调用(如 `engine.with_force_liquidate(True).with_fee_config(0.001)`)
    fn with_force_liquidate<'py>(
        mut slf: PyRefMut<'py, Self>,
        on: bool,
    ) -> PyResult<PyRefMut<'py, Self>> {
        slf.inner.with_force_liquidate(on);
        Ok(slf)
    }

    /// 启用虚拟流动性种子(回测"瞬时对手盘"语义)
    ///
    /// 启用后,应用层每根 bar 调用 `begin_bar(price, symbol)` 即可触发
    /// 撮合引擎的 `clear_book + seed_liquidity`,让策略单"瞬时有对手盘"成交。
    /// 不启用时,BacktestEngine 是纯订单簿撮合(无对手盘,buy 单 → fills=0)。
    ///
    /// Args:
    /// - `half_spread`: 每层价差(绝对价格单位),如 `0.0001 * mid = 10bps`
    /// - `depth_levels`: 每侧挂单层数(典型 5~20)
    /// - `size_per_level`: 每层挂单数量
    ///
    /// 注意:
    /// - 撮合引擎必须实现 `MatchingEngine::seed_liquidity` 方法,否则 seed
    ///   是 no-op(默认 trait 实现,见 `matching::engine::MatchingEngine`)。
    /// - `L1MatchingEngine` / `ImpactedMatchingEngine` 都重写了该方法,
    ///   提供完整实现。
    /// - **0.7.0 起**:`with_seed_liquidity(...)` 设为 **default** 配线,
    ///   用 `with_seed_liquidity_for(instrument, ...)` 设 per-leg 覆写;
    ///   `begin_bar(price, instrument)` 优先 per-leg,fallback default。
    fn with_seed_liquidity(&mut self, half_spread: f64, depth_levels: usize, size_per_level: f64) {
        self.inner
            .with_seed_liquidity(half_spread, depth_levels, size_per_level);
    }

    /// 0.7.0 新增:per-leg 虚拟流动性种子覆写
    ///
    /// 给定 instrument 设置独立的 `SeedLiquidityConfig`,优先于
    /// `with_seed_liquidity(...)` 设的 default。允许 spot 和 perp 各用
    /// 不同 half_spread / depth / size(spot 紧 / perp 松 的真实市场规律)。
    ///
    /// Args:
    /// - `instrument`: 交易品种 dict(由 `spot_instrument()` / `swap_instrument()` 工厂构造)
    /// - `half_spread`: 每层价差
    /// - `depth_levels`: 每侧挂单层数
    /// - `size_per_level`: 每层挂单数量
    ///
    /// Example:
    /// ```python
    /// engine = BacktestEngine(100_000.0)
    /// engine.with_seed_liquidity(0.1, 5, 0.1)                # default
    /// engine.with_seed_liquidity_for(spot_inst, 0.01, 10, 0.5)  # spot 紧
    /// engine.with_seed_liquidity_for(perp_inst, 0.5, 5, 0.1)    # perp 松
    /// ```
    fn with_seed_liquidity_for(
        &mut self,
        instrument: &Bound<'_, PyAny>,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
    ) -> PyResult<()> {
        let inst = super::types::parse_instrument(instrument.cast::<PyDict>()?)?;
        self.inner
            .with_seed_liquidity_for(inst, half_spread, depth_levels, size_per_level);
        Ok(())
    }

    /// 每根 bar 开始时由应用层调用:同步执行 `clear_book + seed_liquidity`
    ///
    /// 必须在 `push_event("order_submitted", ...)` **之前**调用 —— 让对手盘先就位。
    /// 同步执行不入事件队列,纯配置侧操作。
    ///
    /// Args:
    /// - `price`: 当前 bar 的中间价(通常为 `bar.close`)
    /// - `instrument`: 交易品种 dict(由 `spot_instrument()` / `swap_instrument()` 工厂构造)
    ///
    /// 行为:
    /// - 若未调 `with_seed_liquidity` / `with_seed_liquidity_for`:no-op(纯订单簿撮合,buy 单 → fills=0)
    /// - 若已调(任一):`matcher.clear_book_for(instrument)` 只清该 instrument 的
    ///   book(0.7.0 起,**不**再清其他 leg),再 `seed_liquidity(price, ...)` 挂
    ///   限价单。配线优先 per-leg,fallback default。
    #[pyo3(signature = (price, instrument))]
    fn begin_bar(
        &mut self,
        _py: Python<'_>,
        price: f64,
        instrument: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let inst = super::types::parse_instrument(instrument.cast::<PyDict>()?)?;
        self.inner.begin_bar(price, inst);
        Ok(())
    }

    /// 0.7.0 新增:多 leg 同 bar seed(spot + perp 套利场景)
    ///
    /// 在同一根 bar 内对多个 instrument 同时 seed liquidity,常用于 delta-neutral
    /// 套利(spot 和 perp 各自挂对手盘,策略单两边都能成交)。
    ///
    /// Args:
    /// - `legs`: `dict[instrument_dict, price]`,key 是 instrument dict
    ///   (由 `spot_instrument()` / `swap_instrument()` 工厂构造),
    ///   value 是该 leg 的 mid_price
    ///
    /// Example:
    /// ```python
    /// engine.begin_bar_multi({
    ///     spot_instrument("BTC", "USDT"): 100.0,
    ///     swap_instrument("BTC", "USDT", "UsdMargin", 1.0): 200.5,
    /// })
    /// ```
    ///
    /// 与多次 `begin_bar(price, instrument)` 的区别:
    /// - 多次 `begin_bar` 会 bar_id 自增多次 + funding 调度多次 + 末次 rebalance,
    ///   **不**适合多 leg 同 bar
    /// - `begin_bar_multi` 调一次,bar_id +1,funding 调度一次,末次 rebalance
    fn begin_bar_multi(&mut self, _py: Python<'_>, legs: &Bound<'_, PyDict>) -> PyResult<()> {
        let mut parsed: Vec<(axon_core::types::Instrument, f64)> = Vec::with_capacity(legs.len());
        for (key, value) in legs.iter() {
            let inst = super::types::parse_instrument(key.cast::<PyDict>()?)?;
            let price: f64 = value.extract()?;
            parsed.push((inst, price));
        }
        self.inner.begin_bar_multi(parsed);
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

    // ─── 多 Leg 回测 API (T3.8 / T3.6 暴露) ─────────────────────

    /// 设置某 leg 的目标仓位(0.5.0 新增)
    ///
    /// 仅记录策略意图,不主动下单。`BacktestEngine` 不主动根据
    /// `target_position` 发单 —— 由策略层在每根 bar 末读取
    /// `get_target_position` / `get_position` 自行计算 delta 并下单。
    ///
    /// 重复设置同一 instrument 会覆盖前值(语义:"最新 set 生效")。
    ///
    /// Args:
    /// - `instrument`: 由 `spot_instrument()` / `swap_instrument()` 构造的 dict
    /// - `target`: 目标仓位(正=多,负=空,0=清仓)
    #[pyo3(signature = (instrument, target))]
    fn set_target_position(
        &mut self,
        _py: Python<'_>,
        instrument: &Bound<'_, PyAny>,
        target: f64,
    ) -> PyResult<()> {
        let inst = super::types::parse_instrument(instrument.cast::<PyDict>()?)?;
        self.inner.set_target_position(inst, target);
        Ok(())
    }

    /// 查询某 leg 的目标仓位(0.5.0 新增)
    ///
    /// Returns:`None` 表示从未调过 `set_target_position`,否则返回目标值。
    #[pyo3(signature = (instrument))]
    fn get_target_position(
        &self,
        _py: Python<'_>,
        instrument: &Bound<'_, PyAny>,
    ) -> PyResult<Option<f64>> {
        let inst = super::types::parse_instrument(instrument.cast::<PyDict>()?)?;
        Ok(self.inner.get_target_position(&inst))
    }

    /// 查询某 instrument 的当前仓位(0.5.0 新增)
    ///
    /// Returns:当前净持仓(单位 base,正=多,负=空)。未交易过返回 `0.0`。
    #[pyo3(signature = (instrument))]
    fn get_position(&self, _py: Python<'_>, instrument: &Bound<'_, PyAny>) -> PyResult<f64> {
        let inst = super::types::parse_instrument(instrument.cast::<PyDict>()?)?;
        Ok(self.inner.get_position(&inst))
    }

    /// 推入 Mark 价格事件(0.5.0 新增)
    ///
    /// Mark 事件由数据源/策略在 funding 结算点推送,本次 spec 范围**只**
    /// 写入 `mark_cache`,**不**触发 NAV 重采样、**不**做 funding 结算。
    ///
    /// 幂等:同一 instrument 多次 mark 事件,后到的覆盖前到的(最新价生效)。
    ///
    /// Args:
    /// - `instrument`: 目标品种 dict
    /// - `price`: mark 价格
    /// - `timestamp_ns`: 事件纳秒时间戳
    #[pyo3(signature = (instrument, price, timestamp_ns))]
    fn push_mark(
        &mut self,
        _py: Python<'_>,
        instrument: &Bound<'_, PyAny>,
        price: f64,
        timestamp_ns: i64,
    ) -> PyResult<()> {
        let inst = super::types::parse_instrument(instrument.cast::<PyDict>()?)?;
        let mark = MarkEvent {
            instrument: inst,
            mark_price: Price::from_f64(price),
            timestamp: Timestamp::from_nanos(timestamp_ns),
        };
        // 复用现有事件路径,统一经 EventQueue 与 dispatcher
        self.builder.mark(mark.clone());
        // 通过 push_event 路径进队(dispatcher 写入 mark_cache)
        // 实际 MarkEvent 没有对应的 push_event type 字符串,
        // 这里直接用 inner.push_event 绕过 type 字符串协议。
        self.inner.push_event(axon_core::event::Event::Mark(mark));
        Ok(())
    }

    // ─── 0.5.0 新增(Phase C):Funding 结算 ─────────────────────

    /// 推入 Funding 结算事件(0.5.0 新增 Phase C)
    ///
    /// 永续合约资金费率由数据源每 8h(可调)推入,引擎按
    /// `position_qty × funding_rate × mark_price` 累计到 cash
    /// 并写入 `RunResult.total_funding_pnl`。
    ///
    /// spot instrument 收到 funding 会被忽略(spot 无 funding 概念)。
    ///
    /// Args:
    /// - `instrument`: 永续合约 dict(由 `swap_instrument()` 构造;spot 收到会被忽略)
    /// - `funding_rate`: 资金费率(正=long 付,负=long 收)
    /// - `mark_price`: 结算时 mark 价格
    /// - `timestamp_ns`: 事件纳秒时间戳
    #[pyo3(signature = (instrument, funding_rate, mark_price, timestamp_ns))]
    fn push_funding(
        &mut self,
        _py: Python<'_>,
        instrument: &Bound<'_, PyAny>,
        funding_rate: f64,
        mark_price: f64,
        timestamp_ns: i64,
    ) -> PyResult<()> {
        let inst = super::types::parse_instrument(instrument.cast::<PyDict>()?)?;
        self.inner.push_funding(
            inst,
            funding_rate,
            mark_price,
            Timestamp::from_nanos(timestamp_ns),
        );
        Ok(())
    }

    // ─── 0.5.0 新增(Phase D):自动 rebalance ─────────────────────

    /// 启用自动 rebalance 阈值(0.5.0 新增 Phase D)
    ///
    /// 启用后,策略层在每根 bar 末调 `rebalance_to_target()` 即可按
    /// `|target - current| > threshold` 对每个 leg 自动发市价单把
    /// 仓位推到位。多次调可覆盖前值;`with_auto_rebalance_disable()`
    /// 关闭。
    ///
    /// Args:
    /// - `threshold`: 最小 delta(绝对值)。建议 `1e-6` 避免抖动;
    ///   `0.0` 等价"每 tick rebalance"。
    fn with_auto_rebalance(&mut self, threshold: f64) {
        self.inner.with_auto_rebalance(threshold);
    }

    /// 关闭自动 rebalance(回到默认 `None` 状态)
    fn with_auto_rebalance_disable(&mut self) {
        self.inner.with_auto_rebalance_disable();
    }

    /// 手动设置模拟时钟(0.6.0 新增 Phase 2)
    ///
    /// 回测跳秒场景:quantcell 跨 8h 调度 / 测试用例构造特定 timestamp。
    /// 等价 `engine.config.clock.set(ts)`,但**只暴露只写 timestamp**。
    ///
    /// Args:
    /// - `timestamp_ns`: 纳秒时间戳(整数)
    fn set_clock(&mut self, timestamp_ns: i64) {
        self.inner.set_clock(Timestamp::from_nanos(timestamp_ns));
    }

    /// 手动触发 rebalance(0.5.0 新增 Phase D)
    ///
    /// 遍历所有通过 `set_target_position` 设置过的 leg,对
    /// `|target - current| > threshold` 的 leg 发市价单。
    ///
    /// Args:
    /// - `threshold`: 阈值(绝对值)。不传 / 传 `None` 用
    ///   `with_auto_rebalance` 配置的阈值;都没设则不发单。
    ///
    /// Returns:
    /// - 实际发出去的 rebalance 单数(便于统计)
    #[pyo3(signature = (threshold = None))]
    fn rebalance_to_target(&mut self, threshold: Option<f64>) -> u64 {
        self.inner.rebalance_to_target(threshold)
    }

    // ─── 0.6.0 新增(Phase 2):funding 8h 自动调度 ─────────────────

    /// 配置 funding 自动调度 schedule(0.6.0 新增 Phase 2)
    ///
    /// 启用后,`begin_bar` 收尾遍历所有 schedule,若
    /// `last_funding_ts[instrument] + interval_ns <= bar_ts` 合成
    /// `FundingEvent` 推入队列(走 `push_funding` 派发路径)。
    /// 多次调同一 instrument 覆盖前值。
    ///
    /// Args:
    /// - `instrument`: 永续合约 dict(`swap_instrument` 构造)
    /// - `interval_ns`: 结算间隔(ns)。典型 8h = 28_800_000_000_000
    /// - `fixed_rate`: 资金费率(正 = long 付)
    /// - `mark_aware`: 是否用 `mark_cache` 读 mark(默认 True)
    #[pyo3(signature = (instrument, interval_ns, fixed_rate, mark_aware = true))]
    fn with_funding_schedule(
        &mut self,
        _py: Python<'_>,
        instrument: &Bound<'_, PyAny>,
        interval_ns: i64,
        fixed_rate: f64,
        mark_aware: bool,
    ) -> PyResult<()> {
        let inst = super::types::parse_instrument(instrument.cast::<PyDict>()?)?;
        let schedule = FundingSchedule {
            instrument: inst,
            interval_ns,
            fixed_rate,
            mark_aware,
        };
        self.inner.with_funding_schedule(schedule);
        Ok(())
    }

    /// 关闭指定 instrument 的 funding 自动调度(0.6.0 新增 Phase 2)
    ///
    /// Args:
    /// - `instrument`: 永续合约 dict
    fn with_funding_schedule_disable(
        &mut self,
        _py: Python<'_>,
        instrument: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let inst = super::types::parse_instrument(instrument.cast::<PyDict>()?)?;
        self.inner.with_funding_schedule_disable(&inst);
        Ok(())
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

    // ── Stage 3 阶段 B 新增字段 ─────────────────────────────

    /// 完整交易记录(开/平仓配对的 TradeRecord 列表)
    #[getter]
    fn trades<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        // ponytail:简化为 dict 列表(每个 TradeRecord → 8 字段 dict)
        // 若 quantcell 需要 Arrow / numpy 可后续加 trades_arrow
        let list = PyList::empty(py);
        for tr in &self.inner.trades {
            let d = PyDict::new(py);
            d.set_item("timestamp_ns", tr.trade.timestamp.nanos)?;
            d.set_item("price", tr.trade.price.as_f64())?;
            d.set_item("quantity", tr.trade.quantity.as_f64())?;
            d.set_item("buyer_order_id", tr.trade.buyer_order_id)?;
            d.set_item("seller_order_id", tr.trade.seller_order_id)?;
            d.set_item("realized_pnl", tr.realized_pnl as f64 / 1e6)?;
            d.set_item("commission", tr.commission as f64 / 1e6)?;
            d.set_item("net_quantity", tr.net_quantity as f64 / 1e6)?;
            list.append(d)?;
        }
        Ok(list)
    }

    /// 每笔 fill 完整记录(0.7.0 新增)
    ///
    /// 与 `trades` 区别:
    /// - `trades`:round-trip 配对 TradeRecord,只记已平仓(开+平配对)
    /// - `fills_detail`:每笔 `MatchFill` 都记(开仓/同向加仓/平仓/部分 fill 全包含)
    ///
    /// 适用场景:审计每笔成交、partial fill 分析、按 taker/maker 拆分
    /// 不适用场景:算胜率/夏普(那用 `trades` + `win_rate`/`sharpe_ratio`)
    ///
    /// 返回 list[dict],每 dict 7 字段:
    /// - `timestamp_ns`:int(fill 时间)
    /// - `instrument`:tuple(`("spot", base, quote)` 或 `("swap", base, quote, settle, contract_size)`)
    /// - `taker_order_id`:int(吃单方)
    /// - `maker_order_id`:int(挂单方)
    /// - `taker_side`:str("Buy" / "Sell")
    /// - `price`:float
    /// - `quantity`:float
    #[getter]
    fn fills_detail<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for fr in &self.inner.fills_detail {
            let d = PyDict::new(py);
            d.set_item("timestamp_ns", fr.timestamp.nanos)?;
            d.set_item("instrument", instrument_to_tuple(py, &fr.instrument)?)?;
            d.set_item("taker_order_id", fr.taker_order_id)?;
            d.set_item("maker_order_id", fr.maker_order_id)?;
            d.set_item(
                "taker_side",
                match fr.taker_side {
                    axon_core::market::Side::Buy => "Buy",
                    axon_core::market::Side::Sell => "Sell",
                },
            )?;
            d.set_item("price", fr.price.as_f64())?;
            d.set_item("quantity", fr.quantity.as_f64())?;
            list.append(d)?;
        }
        Ok(list)
    }

    /// 累计手续费(f64,按 fill 累计扣除)
    #[getter]
    fn total_fees(&self) -> f64 {
        self.inner.total_fees
    }

    /// NAV 曲线(`[(timestamp_ns, nav), ...]`)
    #[getter]
    fn equity_curve<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for (ts, nav) in &self.inner.equity_curve {
            let tup = (ts.nanos, *nav);
            list.append(tup)?;
        }
        Ok(list)
    }

    /// NAV 历史峰值(用于 max_drawdown_pct)
    #[getter]
    fn nav_peak(&self) -> f64 {
        self.inner.nav_peak
    }

    /// 最大回撤百分比(0~1)
    #[getter]
    fn max_drawdown_pct(&self) -> f64 {
        self.inner.max_drawdown_pct
    }

    /// 胜率(盈利平仓笔数 / 总平仓笔数)
    #[getter]
    fn win_rate(&self) -> f64 {
        self.inner.win_rate
    }

    /// 夏普比率(基于 log return 年化,15m bar 因子 sqrt(35040))
    #[getter]
    fn sharpe_ratio(&self) -> f64 {
        self.inner.sharpe_ratio
    }

    /// 终态持仓快照
    ///
    /// 返回 dict,key 为 `Instrument` 转成的 Python `tuple`(可哈希):
    /// - spot: `("spot", "BTC", "USDT")`
    /// - swap: `("swap", "BTC", "USDT", "usd_margin", 1.0)`
    ///
    /// Python 端可用作 dict key 索引;需要原始字段时访问 tuple 元素即可。
    #[getter]
    fn positions<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (inst, qty) in &self.inner.positions {
            let key = instrument_to_tuple(py, inst)?;
            d.set_item(key, *qty)?;
        }
        Ok(d)
    }

    /// Leg 目标位快照(0.5.0 新增)
    ///
    /// 返回 `{instrument_tuple: target_qty}`,包含本次回测过程中所有
    /// `set_target_position` 设置过的 leg 目标位。Python 端可用 tuple
    /// 索引:
    /// ```python
    /// spot = ("spot", "BTC", "USDT")
    /// result.leg_targets[spot]  # => 1.0
    /// ```
    #[getter]
    fn leg_targets<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (inst, target) in &self.inner.leg_targets {
            let key = instrument_to_tuple(py, inst)?;
            d.set_item(key, *target)?;
        }
        Ok(d)
    }

    /// Mark 价格快照(0.5.0 新增)
    ///
    /// 返回 `{instrument_tuple: mark_price}`,包含本次回测过程中所有
    /// `push_mark` 推入的最新 mark 价(同一 instrument 后到的覆盖前到的)。
    #[getter]
    fn marks<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (inst, price) in &self.inner.marks {
            let key = instrument_to_tuple(py, inst)?;
            d.set_item(key, *price)?;
        }
        Ok(d)
    }

    /// 累计 funding 结算 PnL(0.5.0 新增 Phase C)
    ///
    /// 正值=累计净收(perp short + 正 funding),负值=累计净付。
    /// 来源:`BacktestEngine::handle_funding` 在收到 `Event::Funding` 时按
    /// `position_qty × funding_rate × mark_price` 累加;`final_nav` 已把
    /// 该值包含在 cash 余额中,这里单列出来便于报告/对账。
    #[getter]
    fn total_funding_pnl(&self) -> f64 {
        self.inner.total_funding_pnl
    }

    /// 自动 rebalance 触发的下单次数(0.5.0 新增 Phase D)
    ///
    /// 由 `rebalance_to_target()` 在每根 bar 末/手动触发时,根据
    /// `|target - current| > threshold` 对每个 leg 发市价单的实际
    /// fill 数累加。0 表示本次回测未调用 rebalance 或所有 leg 都已在阈值内。
    #[getter]
    fn rebalances_triggered(&self) -> u64 {
        self.inner.rebalances_triggered
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
        // 阶段 B 新增
        d.set_item("total_fees", self.inner.total_fees)?;
        d.set_item("nav_peak", self.inner.nav_peak)?;
        d.set_item("max_drawdown_pct", self.inner.max_drawdown_pct)?;
        d.set_item("win_rate", self.inner.win_rate)?;
        d.set_item("sharpe_ratio", self.inner.sharpe_ratio)?;
        d.set_item("trades_count", self.inner.trades.len())?;
        d.set_item("equity_curve_points", self.inner.equity_curve.len())?;
        d.set_item("positions", self.positions(py)?)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "RunResult(events={}, accepted={}, rejected={}, fills={}, \
             pnl={:.2}, drawdown={:.2}, nav={:.2}, fees={:.4}, win_rate={:.2}%, sharpe={:.2})",
            self.inner.events_processed,
            self.inner.orders_accepted,
            self.inner.orders_rejected,
            self.inner.fills,
            self.inner.total_pnl,
            self.inner.max_drawdown,
            self.inner.final_nav,
            self.inner.total_fees,
            self.inner.win_rate * 100.0,
            self.inner.sharpe_ratio,
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

/// `Instrument` → Python tuple(可哈希,作 dict key 用)
///
/// - spot: `("spot", "BTC", "USDT")`
/// - swap: `("swap", "BTC", "USDT", "usd_margin", 1.0)`
///
/// tuple 形式比字符串 key 优雅:Python 端用 `result.positions[("spot", "BTC", "USDT")]`
/// 直查,不需要先 split 再 join。
fn instrument_to_tuple<'py>(py: Python<'py>, inst: &Instrument) -> PyResult<Bound<'py, PyTuple>> {
    use axon_core::types::SwapSettle;
    match inst {
        Instrument::Spot(s) => PyTuple::new(
            py,
            [
                "spot".into_pyobject(py)?.into_any(),
                s.base.as_str().into_pyobject(py)?.into_any(),
                s.quote.as_str().into_pyobject(py)?.into_any(),
            ],
        ),
        Instrument::Swap(s) => {
            let settle_str = match s.settle {
                SwapSettle::UsdMargin => "usd_margin",
                SwapSettle::CoinMargin => "coin_margin",
            };
            PyTuple::new(
                py,
                [
                    "swap".into_pyobject(py)?.into_any(),
                    s.base.as_str().into_pyobject(py)?.into_any(),
                    s.quote.as_str().into_pyobject(py)?.into_any(),
                    settle_str.into_pyobject(py)?.into_any(),
                    s.contract_size.into_pyobject(py)?.into_any(),
                ],
            )
        }
    }
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
