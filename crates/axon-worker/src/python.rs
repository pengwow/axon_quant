//! axon-worker Python 绑定
//!
//! 暴露：
//! - `BacktestRunner`：Rust 原生回测编排器（构造 + 注册策略 + run）
//! - `Kernel`：统一交易内核的薄包装（M2），结果与 `BacktestRunner` 等价
//! - `BacktestResult`：回测结果 DTO
//! - `LiveRecovery` + `read_manifest` / `reconcile` / `resume_guard`：崩溃恢复
//!   三原语（journal 重演恢复 OMS / 三路对账 / 恢复门禁）
//!
//! `run()` 内部用 tokio runtime `block_on` 执行 async 回测循环，
//! bars 从 Python `list[dict]` 构造（每根 bar 含 timestamp/open/high/low/close/volume）。

#![cfg(feature = "python")]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

use axon_backtest::engine::EffectiveMarginSpec;
use axon_backtest::margin::{Bracket, LiquidateReason, LiquidationEvent, MarginMode, MarginSpec};
use axon_core::market::bar::Bar;
use axon_core::portfolio::{FillRecord, TradeRecord};
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Price, Quantity, SwapSettle};
use axon_exchange::ExchangeAdapter;
use axon_exchange::adapters::binance::BinanceAdapter;
use axon_exchange::python::config::PyExchangeConfig;
use axon_kernel::{
    BacktestSink, InferenceBackend, InstrumentMode, Journal, Kernel, KernelConfig, KernelResult,
    RLStrategyHost, RunManifest, StrategyHost, build_frames,
};
use axon_oms::AccountingMode;
use axon_rl::{
    Action, ActionSpace, ContinuousActionSpace, FeatureConfig, ObservationSpec, RunningStats,
};
use axon_strategy::impl_::PythonStrategyImpl;
use axon_strategy::python::PyRuleStrategyImpl;
use axon_strategy::trait_::Strategy;

use crate::LiveSink;
use crate::backtest::{BacktestResult, BacktestRunner};
use crate::kernel_host::BarContextSim;
use crate::live_driver::LiveDriver;
use crate::paper_driver::PaperDriver;
use crate::reconcile::{
    LocalSnapshot, ReconcileConfig, ReconcileReport, VenueSnapshot, reconcile as reconcile_impl,
    resume_guard as resume_guard_impl,
};

/// Python 端 BacktestResult（DTO，纯数据）
///
/// 字段与 `axon_backtest::RunResult`（Python 暴露的 `RunResult`）对齐，
/// QuantCell 可复用 `_convert_backtest_result` 的读取契约。
#[pyclass(name = "BacktestResult", module = "axon_worker", skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct PyBacktestResult {
    #[pyo3(get)]
    pub events_processed: u64,
    #[pyo3(get)]
    pub orders_accepted: u64,
    #[pyo3(get)]
    pub orders_rejected: u64,
    #[pyo3(get)]
    pub orders_cancelled: u64,
    #[pyo3(get)]
    pub orders_modified: u64,
    #[pyo3(get)]
    pub fills: u64,
    #[pyo3(get)]
    pub total_pnl: f64,
    #[pyo3(get)]
    pub max_drawdown: f64,
    #[pyo3(get)]
    pub max_drawdown_pct: f64,
    #[pyo3(get)]
    pub nav_peak: f64,
    #[pyo3(get)]
    pub final_nav: f64,
    #[pyo3(get)]
    pub total_fees: f64,
    #[pyo3(get)]
    pub total_funding_pnl: f64,
    #[pyo3(get)]
    pub win_rate: f64,
    #[pyo3(get)]
    pub sharpe_ratio: f64,
    #[pyo3(get)]
    pub duration_secs: f64,
    #[pyo3(get)]
    pub final_time_ns: i64,
    #[pyo3(get)]
    pub rebalances_triggered: u64,
    #[pyo3(get)]
    pub n_bars: u64,
    #[pyo3(get)]
    pub n_strategies: u32,
    /// 每 bar 末 NAV 曲线（`[(timestamp_ns, nav), ...]`）
    #[pyo3(get)]
    pub bar_nav_curve: Vec<(i64, f64)>,
    /// 每 bar 末 balance 存款曲线（`[(timestamp_ns, balance), ...]`）
    #[pyo3(get)]
    pub bar_balance_curve: Vec<(i64, f64)>,
    /// 稀疏 NAV 曲线（`[(timestamp_ns, nav), ...]`）
    #[pyo3(get)]
    pub equity_curve: Vec<(i64, f64)>,
    /// round-trip 交易记录（自定义 getter → list[dict]）
    pub trades: Vec<TradeRecord>,
    /// 每笔 fill 明细（自定义 getter → list[dict]）
    pub fills_detail: Vec<FillRecord>,
    /// 终态持仓（自定义 getter → dict[tuple, float]）
    pub final_positions: Vec<(Instrument, f64)>,
    /// 强平事件时间线（自定义 getter → list[dict]）
    pub liquidation_events: Vec<LiquidationEvent>,
    /// 穿仓兜底累计额（保险基金语义）
    #[pyo3(get)]
    pub insurance_fund_loss: f64,
    /// 判定价口径（v1 固定 `"close"`）
    #[pyo3(get)]
    pub price_basis: String,
    /// 是否启用保证金账务
    #[pyo3(get)]
    pub margin_applied: bool,
    /// 实际生效的每品种规格（自定义 getter → list[dict]）
    pub margin_specs_effective: Vec<EffectiveMarginSpec>,
    /// 强平事件条数
    #[pyo3(get)]
    pub liquidation_count: u32,
    /// 因可用保证金不足而被缩量的开仓次数
    #[pyo3(get)]
    pub margin_shortfall_count: u32,
}

impl From<&BacktestResult> for PyBacktestResult {
    fn from(r: &BacktestResult) -> Self {
        Self {
            events_processed: r.events_processed,
            orders_accepted: r.orders_accepted,
            orders_rejected: r.orders_rejected,
            orders_cancelled: r.orders_cancelled,
            orders_modified: r.orders_modified,
            fills: r.fills,
            total_pnl: r.total_pnl,
            max_drawdown: r.max_drawdown,
            max_drawdown_pct: r.max_drawdown_pct,
            nav_peak: r.nav_peak,
            final_nav: r.final_nav,
            total_fees: r.total_fees,
            total_funding_pnl: r.total_funding_pnl,
            win_rate: r.win_rate,
            sharpe_ratio: r.sharpe_ratio,
            duration_secs: r.duration_secs,
            final_time_ns: r.final_time_ns,
            rebalances_triggered: r.rebalances_triggered,
            n_bars: r.n_bars,
            n_strategies: r.n_strategies,
            bar_nav_curve: r
                .bar_nav_curve
                .iter()
                .map(|(ts, nav)| (ts.nanos, *nav))
                .collect(),
            bar_balance_curve: r
                .bar_balance_curve
                .iter()
                .map(|(ts, bal)| (ts.nanos, *bal))
                .collect(),
            equity_curve: r
                .equity_curve
                .iter()
                .map(|(ts, nav)| (ts.nanos, *nav))
                .collect(),
            trades: r.trades.clone(),
            fills_detail: r.fills_detail.clone(),
            final_positions: r.final_positions.clone(),
            liquidation_events: r.liquidation_events.clone(),
            insurance_fund_loss: r.insurance_fund_loss,
            price_basis: r.price_basis.clone(),
            margin_applied: r.margin_applied,
            margin_specs_effective: r.margin_specs_effective.clone(),
            liquidation_count: r.liquidation_count,
            margin_shortfall_count: r.margin_shortfall_count,
        }
    }
}

impl PyBacktestResult {
    /// 从内核 `KernelResult` 构造（字段口径与 `BacktestResult` 一致）
    ///
    /// `n_bars` 取内核已处理帧数（= 网格长度），与 `BacktestRunner` 的 `grid_len` 同源；
    /// `duration_secs` 用内核自己的墙钟（引擎 `run()` 内部的 `duration` 只覆盖收尾瞬间）。
    fn from_kernel(r: &KernelResult, n_strategies: u32) -> Self {
        let run = &r.run;
        Self {
            events_processed: run.events_processed,
            orders_accepted: run.orders_accepted,
            orders_rejected: run.orders_rejected,
            orders_cancelled: run.orders_cancelled,
            orders_modified: run.orders_modified,
            fills: run.fills,
            total_pnl: run.total_pnl,
            max_drawdown: run.max_drawdown,
            max_drawdown_pct: run.max_drawdown_pct,
            nav_peak: run.nav_peak,
            final_nav: run.final_nav,
            total_fees: run.total_fees,
            total_funding_pnl: run.total_funding_pnl,
            win_rate: run.win_rate,
            sharpe_ratio: run.sharpe_ratio,
            duration_secs: r.duration.as_secs_f64(),
            final_time_ns: run.final_time.nanos,
            rebalances_triggered: run.rebalances_triggered,
            n_bars: r.bars_processed,
            n_strategies,
            bar_nav_curve: run
                .bar_nav_curve
                .iter()
                .map(|(ts, nav)| (ts.nanos, *nav))
                .collect(),
            bar_balance_curve: run
                .bar_balance_curve
                .iter()
                .map(|(ts, bal)| (ts.nanos, *bal))
                .collect(),
            equity_curve: run
                .equity_curve
                .iter()
                .map(|(ts, nav)| (ts.nanos, *nav))
                .collect(),
            trades: run.trades.clone(),
            fills_detail: run.fills_detail.clone(),
            final_positions: run
                .positions
                .iter()
                .map(|(inst, qty)| (inst.clone(), *qty))
                .collect(),
            liquidation_events: run.liquidation_events.clone(),
            insurance_fund_loss: run.insurance_fund_loss,
            price_basis: run.price_basis.clone(),
            margin_applied: run.margin_applied,
            margin_specs_effective: run.margin_specs_effective.clone(),
            liquidation_count: run.liquidation_count,
            margin_shortfall_count: run.margin_shortfall_count,
        }
    }
}

#[pymethods]
impl PyBacktestResult {
    /// 完整交易记录（`list[dict]`，字段与 Python 端 `RunResult.trades` 一致）
    #[getter]
    fn trades<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for tr in &self.trades {
            let d = PyDict::new(py);
            d.set_item("timestamp_ns", tr.trade.timestamp.nanos)?;
            d.set_item("price", tr.trade.price.as_f64())?;
            d.set_item("quantity", tr.trade.quantity.as_f64())?;
            d.set_item("buyer_order_id", tr.trade.buyer_order_id)?;
            d.set_item("seller_order_id", tr.trade.seller_order_id)?;
            d.set_item("realized_pnl", tr.realized_pnl as f64 / 1e6)?;
            d.set_item("commission", tr.commission as f64 / 1e6)?;
            d.set_item("net_quantity", tr.net_quantity as f64 / 1e6)?;
            d.set_item("entry_price", tr.entry_price as f64 / 1e6)?;
            d.set_item("entry_time_ns", tr.entry_time_ns)?;
            // 共享资金池模式需按品种拆分成交，故补 instrument（同 fills_detail 口径）
            d.set_item("instrument", instrument_to_tuple(py, &tr.instrument)?)?;
            // fill 关联：开仓 fill id 列表（同向加仓多条）与触发平仓的 fill id
            d.set_item("entry_fill_ids", tr.entry_fill_ids.clone())?;
            d.set_item("exit_fill_ids", tr.exit_fill_ids.clone())?;
            list.append(d)?;
        }
        Ok(list)
    }

    /// 每笔 fill 明细（`list[dict]`）
    #[getter]
    fn fills_detail<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for fr in &self.fills_detail {
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
            // fill 全局唯一 id（= fills_detail 下标），供 trades 的 entry_fill_ids/exit_fill_id 关联
            d.set_item("fill_id", fr.fill_id)?;
            list.append(d)?;
        }
        Ok(list)
    }

    /// 终态持仓快照（`dict[tuple, float]`，与 Python 端 `RunResult.positions` 一致）
    #[getter]
    fn final_positions<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (inst, qty) in &self.final_positions {
            d.set_item(instrument_to_tuple(py, inst)?, *qty)?;
        }
        Ok(d)
    }

    /// 强平事件时间线（`list[dict]`，未启用保证金时为空列表）
    #[getter]
    fn liquidation_events<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for e in &self.liquidation_events {
            let d = PyDict::new(py);
            d.set_item("timestamp_ns", e.timestamp_ns)?;
            d.set_item("symbol", &e.symbol)?;
            d.set_item(
                "margin_mode",
                match e.margin_mode {
                    MarginMode::Isolated => "isolated",
                    MarginMode::Cross => "cross",
                },
            )?;
            d.set_item("tier_from", e.tier_from)?;
            d.set_item("tier_to", e.tier_to)?;
            d.set_item("reduced_qty", e.reduced_qty)?;
            d.set_item("price", e.price)?;
            d.set_item("fee", e.fee)?;
            d.set_item(
                "reason",
                match e.reason {
                    LiquidateReason::Partial => "partial",
                    LiquidateReason::Depleted => "depleted",
                },
            )?;
            d.set_item("price_basis", e.price_basis)?;
            list.append(d)?;
        }
        Ok(list)
    }

    /// 实际生效的保证金规格（`list[dict]`，未启用保证金时为空列表）
    ///
    /// 字段名与前端契约一致：symbol / leverage / margin_mode / bracket_source。
    #[getter]
    fn margin_specs_effective<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for s in &self.margin_specs_effective {
            let d = PyDict::new(py);
            d.set_item("symbol", &s.symbol)?;
            d.set_item("leverage", s.leverage)?;
            d.set_item("margin_mode", &s.margin_mode)?;
            d.set_item("bracket_source", &s.bracket_source)?;
            list.append(d)?;
        }
        Ok(list)
    }

    pub fn __repr__(&self) -> String {
        format!(
            "BacktestResult(bars={}, strategies={}, fills={}, final_nav={:.2}, pnl={:.2})",
            self.n_bars, self.n_strategies, self.fills, self.final_nav, self.total_pnl
        )
    }
}

/// Instrument → Python tuple（与 axon-backtest 的 `instrument_to_tuple` 对齐）
///
/// spot: `("spot", base, quote)`；swap: `("swap", base, quote, settle, contract_size)`
fn instrument_to_tuple<'py>(py: Python<'py>, inst: &Instrument) -> PyResult<Bound<'py, PyTuple>> {
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
            let settle = match s.settle {
                axon_core::types::SwapSettle::UsdMargin => "usd_margin",
                axon_core::types::SwapSettle::CoinMargin => "coin_margin",
            };
            PyTuple::new(
                py,
                [
                    "swap".into_pyobject(py)?.into_any(),
                    s.base.as_str().into_pyobject(py)?.into_any(),
                    s.quote.as_str().into_pyobject(py)?.into_any(),
                    settle.into_pyobject(py)?.into_any(),
                    s.contract_size.into_pyobject(py)?.into_any(),
                ],
            )
        }
    }
}

/// Python 端 BacktestRunner（Rust 原生回测编排器）
///
/// 用法：
/// ```python
/// from axon_quant import strategy, worker
/// runner = worker.BacktestRunner(initial_cash=100000.0)
/// runner.add_strategy(strategy.RuleStrategyImpl("ma", "BTCUSDT"))
/// result = runner.run(bars=[{"timestamp": ns, "open":..., ...}, ...], state_by_symbol={})
/// ```
#[pyclass(name = "BacktestRunner", module = "axon_worker")]
pub struct PyBacktestRunner {
    inner: Mutex<BacktestRunner>,
    runtime: tokio::runtime::Runtime,
}

#[pymethods]
impl PyBacktestRunner {
    #[new]
    #[pyo3(signature = (initial_cash=100000.0, futures=false, force_liquidate=false, settle=None, contract_size=1.0))]
    fn new(
        initial_cash: f64,
        futures: bool,
        force_liquidate: bool,
        settle: Option<String>,
        contract_size: f64,
    ) -> PyResult<Self> {
        let runtime = tokio::runtime::Runtime::new().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("创建 tokio runtime 失败: {e}"))
        })?;
        let mut runner = BacktestRunner::simple(initial_cash);
        if futures {
            // 保证金结算方式：`usd_margin`（U 本位，默认）/ `coin_margin`（币本位）；
            // 合约乘数默认 1.0。注意 `contract_size` 目前只是品种元数据 ——
            // axon-backtest 引擎尚未把乘数计入名义价值/盈亏（engine.rs 里挂着 0.8.0 待办），
            // 币本位只有结算方式被如实标注，账务仍是近似。
            runner = runner.with_futures(parse_settle(settle.as_deref())?, contract_size);
        }
        // 回测结束 EOD 强制平仓
        runner = runner.with_force_liquidate(force_liquidate);
        Ok(Self {
            inner: Mutex::new(runner),
            runtime,
        })
    }

    /// 注册一个策略
    ///
    /// 支持两种类型：
    /// - `strategy.RuleStrategyImpl` 实例（Rust 内置双均线策略）
    /// - 任意 Python 对象，需实现 `decide(bar_dict)` 或 `on_bar(bar_dict)` 方法，
    ///   返回含 `action_type`/`confidence`/`target_position` 的 dict 或对象
    #[pyo3(signature = (strategy, name=None, symbol=None))]
    fn add_strategy(
        &self,
        strategy: &Bound<'_, PyAny>,
        name: Option<String>,
        symbol: Option<String>,
    ) -> PyResult<()> {
        let (_, s) = wrap_strategy(strategy, name, symbol)?;

        self.inner
            .lock()
            .map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("BacktestRunner lock 失败: {e}"))
            })?
            .add_strategy(s);
        Ok(())
    }

    /// 注入每品种保证金规格（空 dict = 不启用保证金模拟）
    ///
    /// 形如：
    /// ```python
    /// runner.with_margin({
    ///     "BTC/USDT": {
    ///         "leverage": 10.0,
    ///         "mode": "cross",              # "cross" / "isolated"
    ///         "brackets": [{"notional_floor": 0.0, "notional_cap": 50000.0,
    ///                       "mmr": 0.004, "max_leverage": 125.0}],
    ///     }
    /// })
    /// ```
    fn with_margin(&self, specs: &Bound<'_, PyDict>) -> PyResult<()> {
        let parsed = parse_margin_specs(specs)?;

        // 原地替换：保留已注册策略与 futures 品种配置
        self.inner
            .lock()
            .map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("BacktestRunner lock 失败: {e}"))
            })?
            .set_margin_specs(parsed);
        Ok(())
    }

    /// 执行回测（同步阻塞，内部 block_on async 循环）
    ///
    /// 单品种与多品种共享账户统一走这一条路径：单品种只需传一个 symbol 的完整
    /// bar 列表（无 `None` 缺口）。
    ///
    /// - `bars_by_symbol`: `dict[str, list[dict | None]]`，各 symbol 的**等长**逐网格点
    ///   bar 列表；`None` 表示该网格点无真实 bar（价格由 Rust 侧前向填充参与盯市，
    ///   但不触发策略决策）
    /// - `states_by_symbol`: dict[str, dict | list[dict]]；值为 list 时按**逐 bar** 注入
    ///   （第 i 项对应第 i 个网格点），值为单对象时展开为「每 bar 同值」的便捷用法
    /// - `funding_by_symbol`: dict[str, dict[int, tuple(rate, mark_price)]]，资金费查找表
    ///   （键为 bar 纳秒时间戳；命中即入队结算）。现货回测传 None 即可
    /// - `capital_budget`: `float | None`，资金约束预算（`initial_cash × leverage`）；
    ///   `None` 表示不启用约束。单品种回测传 None
    #[pyo3(signature = (bars_by_symbol, states_by_symbol=None, funding_by_symbol=None, capital_budget=None))]
    fn run(
        &self,
        bars_by_symbol: &Bound<'_, PyDict>,
        states_by_symbol: Option<Bound<'_, PyDict>>,
        funding_by_symbol: Option<Bound<'_, PyDict>>,
        capital_budget: Option<f64>,
    ) -> PyResult<PyBacktestResult> {
        // 1) dict[str, list[dict|None]] → BTreeMap<String, Vec<Option<Bar>>>
        let bars_map = parse_bars(bars_by_symbol)?;
        if bars_map.is_empty() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "bars_by_symbol 不能为空",
            ));
        }
        let grid_len = bars_map.values().map(|v| v.len()).max().unwrap_or(0);

        // 2) states_by_symbol（逐 bar；长度不齐时按网格长度补齐「每 bar 同值」）
        let states = parse_states(states_by_symbol.as_ref(), grid_len)?;

        // 3) funding_by_symbol → BTreeMap<String, BTreeMap<i64, (f64, f64)>>
        let funding = parse_funding(funding_by_symbol.as_ref())?;

        // 4) block_on 执行 async 回测
        let runner = self.inner.lock().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("BacktestRunner lock 失败: {e}"))
        })?;
        let result =
            self.runtime
                .block_on(runner.run(&bars_map, &states, &funding, capital_budget));

        Ok((&result).into())
    }
}

/// 已注册的 RL 策略规格
///
/// 只保存「构造一次 Host 所需」的静态信息；`RLStrategyHost` 自带逐 bar 历史窗口，
/// 因此每次 `Kernel.run` 都按本规格重建 Host，窗口不跨回测复用。
struct RlStrategySpec {
    /// Python 预测回调：`obs -> target_ratio ∈ [-1, 1]`
    predict: Py<PyAny>,
    /// 模型标识（进入 journal / 日志）
    model_id: String,
    /// 观测组装规格（与训练侧同一份实现）
    spec: ObservationSpec,
    /// 本策略负责的品种
    symbol: Instrument,
    /// 训练时的动作空间（必须覆盖 `[-1, 1]`，见 `ensure_ratio_passthrough`）
    action_space: ActionSpace,
}

/// `add_rl_strategy` 的统一注册链（Kernel / LiveDriver / PaperDriver 同口径）
///
/// 校验顺序与文案三处一致：predict 可调用 → 标识非空 → symbol 带分隔符
/// （无分隔符会退化成默认品种，导致 Host 过滤不到 bar）→ `ObservationSpec` →
/// stats 逐特征对齐 → 动作空间覆盖 `[-1, 1]`。
#[allow(clippy::too_many_arguments)]
fn build_rl_spec(
    predict: &Bound<'_, PyAny>,
    model_id: &str,
    spec_version: &str,
    window_size: usize,
    features: &Bound<'_, PyAny>,
    action_space: Option<&Bound<'_, PyDict>>,
    symbol: &str,
    mode: &InstrumentMode,
    stats: Option<&Bound<'_, PyAny>>,
) -> PyResult<RlStrategySpec> {
    if !predict.is_callable() {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "predict 必须是可调用对象 (list[float]) -> float",
        ));
    }
    if model_id.is_empty() || spec_version.is_empty() {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "model_id / spec_version 不能为空",
        ));
    }
    if !symbol.contains('/') && !symbol.contains('-') {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "symbol 需要 BASE/QUOTE 形式（如 BTC/USDT），收到: {symbol}"
        )));
    }

    let mut spec = ObservationSpec::new(window_size, parse_features(features)?)
        .map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("ObservationSpec 构造失败: {e}"))
        })?
        .with_version(spec_version);

    // 训练期统计量：观测组装按特征下标取统计，条数不符会在推理时越界，
    // 因此注册阶段就拒绝。空 list 与 None 同义（保持全空）。
    let stats = match stats {
        Some(stats) => parse_normalizer_stats(stats)?,
        None => Vec::new(),
    };
    if !stats.is_empty() {
        if stats.len() != spec.features.len() {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "stats 条数必须等于特征数 {}（每特征一条），收到 {}",
                spec.features.len(),
                stats.len()
            )));
        }
        spec.normalizer.stats = stats;
    }

    let action_space = match action_space {
        Some(dict) => axon_rl::python::parse_action_space(dict)?,
        None => ActionSpace::Continuous(ContinuousActionSpace::new(-1.0, 1.0)),
    };
    ensure_ratio_passthrough(&action_space)?;

    Ok(RlStrategySpec {
        predict: predict.clone().unbind(),
        model_id: model_id.to_string(),
        spec,
        symbol: mode.instrument(symbol),
        action_space,
    })
}

/// Python 预测回调 → 内核推理后端
///
/// 内核只认 `obs -> Action`；SB3 的 `predict` 在 Python 侧已经包成
/// `list[float] -> target_ratio`，这里把 ratio 放进连续动作的第一个分量。
/// 动作空间固定覆盖 `[-1, 1]`，所以
/// `ratio → Action::continuous([ratio]) → decode_target_ratio` 是恒等映射
/// （`decode_target_ratio` 只做 clip）。
struct PyRatioBackend {
    predict: Py<PyAny>,
    model_id: String,
    /// 最近一次成功推理的目标占比
    ///
    /// 推理抛异常 / 返回非有限值时沿用上次结果：`SetTarget` 契约没有「Hold」，
    /// 退回 0.0 会强制平仓，把一次模型异常放大成清仓动作。
    last_ratio: f64,
}

impl PyRatioBackend {
    fn new(predict: Py<PyAny>, model_id: String) -> Self {
        Self {
            predict,
            model_id,
            last_ratio: 0.0,
        }
    }
}

impl InferenceBackend for PyRatioBackend {
    fn infer(&mut self, obs: &[f64]) -> Action {
        let ratio = Python::attach(|py| {
            self.predict
                .call1(py, (obs,))
                .and_then(|result| result.extract::<f64>(py))
        });
        match ratio {
            Ok(value) if value.is_finite() => self.last_ratio = value.clamp(-1.0, 1.0),
            Ok(value) => tracing::warn!(
                model = %self.model_id,
                value,
                "rl backend: predict 返回非有限值，沿用上次目标"
            ),
            Err(err) => tracing::warn!(
                model = %self.model_id,
                error = %err,
                "rl backend: predict 调用失败，沿用上次目标"
            ),
        }
        Action::continuous(vec![self.last_ratio])
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// 断言动作空间能无损承载 `[-1, 1]` 的目标占比
///
/// 离散空间会把 ratio 量化到 bin，窄于 `[-1, 1]` 的连续空间会裁剪，
/// 两者都会让 `ratio → Action → decode_target_ratio` 不再是恒等映射。
/// 与其造一条有损往返，不如在注册时直接拒绝。
fn ensure_ratio_passthrough(space: &ActionSpace) -> PyResult<()> {
    match space {
        ActionSpace::Continuous(space) if space.min <= -1.0 && space.max >= 1.0 => Ok(()),
        _ => Err(pyo3::exceptions::PyValueError::new_err(
            "action_space 必须是覆盖 [-1, 1] 的连续空间（如 \
             {'type': 'continuous', 'min': -1.0, 'max': 1.0}）；\
             离散或更窄的空间会量化/裁剪 predict 返回值",
        )),
    }
}

/// Python `list[dict]` → `Vec<FeatureConfig>`
///
/// 复用 `FeatureConfig` 的 serde 表示（`source` / `normalizer` 为外部标记枚举），
/// 避免在绑定层再手写一套枚举解析。
fn parse_features(features: &Bound<'_, PyAny>) -> PyResult<Vec<FeatureConfig>> {
    let json_mod = features.py().import("json")?;
    let json_str: String = json_mod.call_method1("dumps", (features,))?.extract()?;
    serde_json::from_str(&json_str).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "features 解析失败（期望 list[dict]）: {e}"
        ))
    })
}

/// Python `list[dict]` → `Vec<RunningStats>`
///
/// 与 `TradingEnv.observation_stats()` 的输出同格式（`RunningStats` 的 serde 表示）。
fn parse_normalizer_stats(stats: &Bound<'_, PyAny>) -> PyResult<Vec<RunningStats>> {
    let json_mod = stats.py().import("json")?;
    let json_str: String = json_mod.call_method1("dumps", (stats,))?.extract()?;
    serde_json::from_str(&json_str).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "stats 解析失败（期望 list[dict]，元素含 mean/m2/min/max/count/median_buffer）: {e}"
        ))
    })
}

/// Python 端统一交易内核（薄包装）
///
/// 只做类型转换与转发：参数解析复用 `BacktestRunner` 的同一组辅助函数，
/// 编排语义全部在内核里。结果与 `BacktestRunner` 等价（`BacktestSink` 的
/// 占比 → 数量换算 + `BarContextSim` 的逆运算互逆，见 `kernel_host`）。
///
/// ```python
/// from axon_quant import strategy, worker
/// k = worker.Kernel(initial_cash=100000.0)
/// k.add_strategy(strategy.RuleStrategyImpl("ma", "BTCUSDT"))
/// result = k.run(bars_by_symbol={"BTCUSDT": [...]}, states_by_symbol={...})
/// ```
#[pyclass(name = "Kernel", module = "axon_worker")]
pub struct PyKernel {
    initial_cash: f64,
    force_liquidate: bool,
    instrument_mode: InstrumentMode,
    interval: String,
    /// 手续费率(已解析默认值并校验,`None` 在构造时即折算为默认值)
    fee_rate: f64,
    margin_specs: Mutex<HashMap<String, MarginSpec>>,
    /// 已注册策略及其交易 symbol（symbol 用于构造 Host 的品种过滤）
    strategies: Mutex<Vec<(String, Arc<dyn Strategy>)>>,
    /// 已注册的 RL 策略（每次 `run` 按规格重建 `RLStrategyHost`）
    rl_strategies: Mutex<Vec<RlStrategySpec>>,
    manifest: Mutex<axon_kernel::persist::RunManifest>,
    last_result: Mutex<Option<PyBacktestResult>>,
    /// 驱动策略 `decide()` 的 runtime；`Kernel` 主回路本身是同步的
    runtime: tokio::runtime::Runtime,
}

#[pymethods]
impl PyKernel {
    #[new]
    #[pyo3(signature = (initial_cash=100000.0, futures=false, force_liquidate=false, settle=None, contract_size=1.0, interval="1m".to_string(), fee_rate=None))]
    fn new(
        initial_cash: f64,
        futures: bool,
        force_liquidate: bool,
        settle: Option<String>,
        contract_size: f64,
        interval: String,
        fee_rate: Option<f64>,
    ) -> PyResult<Self> {
        let runtime = tokio::runtime::Runtime::new().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("创建 tokio runtime 失败: {e}"))
        })?;
        // `None` = 引擎默认费率(0.001);显式传入才校验,
        // 负数或 NaN/inf 会让手续费累计失真,直接在构造期拒绝
        let fee_rate = match fee_rate {
            None => KernelConfig::default().fee_rate,
            Some(rate) if rate.is_finite() && rate >= 0.0 => rate,
            Some(rate) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "fee_rate 必须是有限非负数，收到: {rate}"
                )));
            }
        };
        Ok(Self {
            initial_cash,
            force_liquidate,
            // 与 `PyBacktestRunner::new` 同规则：futures 才谈结算方式与乘数
            instrument_mode: build_instrument_mode(futures, settle.as_deref(), contract_size)?,
            interval,
            fee_rate,
            margin_specs: Mutex::new(HashMap::new()),
            strategies: Mutex::new(Vec::new()),
            rl_strategies: Mutex::new(Vec::new()),
            manifest: Mutex::new(axon_kernel::persist::RunManifest::default()),
            last_result: Mutex::new(None),
            runtime,
        })
    }

    /// 注册一个策略（包装规则与 `BacktestRunner.add_strategy` 完全一致）
    #[pyo3(signature = (strategy, name=None, symbol=None))]
    fn add_strategy(
        &self,
        strategy: &Bound<'_, PyAny>,
        name: Option<String>,
        symbol: Option<String>,
    ) -> PyResult<()> {
        let (symbol, s) = wrap_strategy(strategy, name, symbol)?;
        kernel_lock(&self.strategies, "strategies")?.push((symbol, s));
        Ok(())
    }

    /// 注册一个 RL 策略（Python 推理回调 → 内核 `RLStrategyHost`）
    ///
    /// `predict(obs: list[float]) -> float` 返回目标权益占比，内核把它原样作为
    /// `SetTarget.target_ratio`：`ratio → Action → decode_target_ratio` 必须是恒等
    /// 映射，因此 `action_space` 只接受覆盖 `[-1, 1]` 的连续空间（默认
    /// `{"type": "continuous", "min": -1.0, "max": 1.0}`）。
    ///
    /// `features` 是 `ObservationSpec` 的特征配置（与训练侧同一份实现），元素形如：
    /// ```python
    /// {"name": "close", "source": {"PriceField": "close"},
    ///  "normalizer": "None", "clip_range": None}
    /// ```
    /// `source` / `normalizer` 使用 serde 的外部标记枚举写法
    /// （`FeatureSource` / `NormalizerType`）。
    ///
    /// `symbol` 必须带分隔符（`BTC/USDT` / `BTC-USDT`）：内核按它构造 `Instrument`，
    /// 无分隔符会退化成默认品种，导致 Host 过滤不到任何 bar。
    ///
    /// `stats` 是训练期观测归一化统计量（`TradingEnv.observation_stats()` 的 JSON
    /// 反序列化结果），`None` 时保持全空（Z-Score 恒为 0，与训练分布不一致）。
    /// 传了就按特征逐条对齐，条数必须等于特征数。
    // 绑定层签名由 Python 调用方决定（7 个必填 + 2 个可选），拆结构体会破坏现有位置参数
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (predict, model_id, spec_version, window_size, features, action_space=None, symbol="BTC/USDT", stats=None))]
    fn add_rl_strategy(
        &self,
        predict: &Bound<'_, PyAny>,
        model_id: &str,
        spec_version: &str,
        window_size: usize,
        features: &Bound<'_, PyAny>,
        action_space: Option<&Bound<'_, PyDict>>,
        symbol: &str,
        stats: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let spec = build_rl_spec(
            predict,
            model_id,
            spec_version,
            window_size,
            features,
            action_space,
            symbol,
            &self.instrument_mode,
            stats,
        )?;
        kernel_lock(&self.rl_strategies, "rl strategies")?.push(spec);
        Ok(())
    }

    /// 注入每品种保证金规格（解析逻辑与 `BacktestRunner.with_margin` 共用）
    fn with_margin(&self, specs: &Bound<'_, PyDict>) -> PyResult<()> {
        *kernel_lock(&self.margin_specs, "margin")? = parse_margin_specs(specs)?;
        Ok(())
    }

    /// 设置重演凭证（透传 `RunManifest`，默认 code_version 取 crate 版本）
    #[pyo3(signature = (config_hash=None, data_hash=None, seed=0))]
    fn set_manifest(
        &self,
        config_hash: Option<String>,
        data_hash: Option<String>,
        seed: u64,
    ) -> PyResult<()> {
        let manifest = axon_kernel::persist::RunManifest {
            config_hash: config_hash.unwrap_or_default(),
            data_hash: data_hash.unwrap_or_default(),
            seed,
            ..axon_kernel::persist::RunManifest::default()
        };
        *kernel_lock(&self.manifest, "manifest")? = manifest;
        Ok(())
    }

    /// 参数语义与 `BacktestRunner.run` 完全一致
    ///
    /// - `bars_by_symbol`: `dict[str, list[dict | None]]`，各 symbol 等长逐网格点
    /// - `states_by_symbol`: dict[str, dict | list[dict]]；list 按逐 bar 注入，
    ///   单对象展开为「每 bar 同值」。state 经 `build_frames` 进帧，再由内核
    ///   按 `(帧时间戳, 品种)` 回填到 `BarClosedEvent.state`
    /// - `funding_by_symbol`: `dict[str, dict[int, tuple(rate, mark_price)]]`
    /// - `capital_budget`: `float | None`（`initial_cash × leverage`）
    /// - `journal_path`: `str | None`，首跑录制：传入则把本次运行收到的全部外部
    ///   输入（含当帧逐 bar state）落 journal，供 `LiveRecovery.resume` 崩溃恢复
    ///   重演 / 契约测试构造 fixture。对照重演：`LiveRecovery` 构造参数必须与
    ///   本侧内核参数一致（initial_cash / interval / fee_rate / 品种口径）。
    #[pyo3(signature = (bars_by_symbol, states_by_symbol=None, funding_by_symbol=None, capital_budget=None, journal_path=None))]
    fn run(
        &self,
        py: Python<'_>,
        bars_by_symbol: &Bound<'_, PyDict>,
        states_by_symbol: Option<Bound<'_, PyDict>>,
        funding_by_symbol: Option<Bound<'_, PyDict>>,
        capital_budget: Option<f64>,
        journal_path: Option<String>,
    ) -> PyResult<PyBacktestResult> {
        let bars_map = parse_bars(bars_by_symbol)?;
        if bars_map.is_empty() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "bars_by_symbol 不能为空",
            ));
        }
        let grid_len = bars_map.values().map(|v| v.len()).max().unwrap_or(0);
        let states = parse_states(states_by_symbol.as_ref(), grid_len)?;
        let funding = parse_funding(funding_by_symbol.as_ref())?;

        let frames = build_frames(&bars_map, &self.instrument_mode, Some(&states))
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        // 屏障的期望品种全集 = 本次网格里出现过的全部 instrument
        let expected_instruments: HashSet<Instrument> = frames
            .iter()
            .flat_map(|frame| frame.bars.iter().map(|(instrument, _)| instrument.clone()))
            .collect();

        // 先克隆出本次运行需要的状态，避免持锁跑完整段回测
        let strategies = kernel_lock(&self.strategies, "strategies")?.clone();
        let margin_specs = kernel_lock(&self.margin_specs, "margin")?.clone();
        let manifest = kernel_lock(&self.manifest, "manifest")?.clone();

        let config = KernelConfig {
            initial_cash: self.initial_cash,
            fee_rate: self.fee_rate,
            force_liquidate: self.force_liquidate,
            margin_specs,
            instrument_mode: self.instrument_mode,
            interval: self.interval.clone(),
            capital_budget,
            funding_by_symbol: funding,
            expected_instruments,
            // rebalance_threshold / seed 走默认值（0.001 / 0.0005 / 5 / 100.0），
            // 与 `BacktestRunner` 的模块常量逐位一致（parity 前提）
            ..KernelConfig::default()
        };
        let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
        kernel.set_manifest(manifest);
        if let Some(path) = journal_path {
            kernel.attach_journal(Path::new(&path)).map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("创建 journal 失败 {path}: {e}"))
            })?;
        }

        let handle = self.runtime.handle().clone();
        for (symbol, strategy) in &strategies {
            kernel.add_host(Box::new(BarContextSim::new(
                strategy.clone(),
                self.instrument_mode.instrument(symbol),
                handle.clone(),
            )));
        }

        // RL host 每次运行重建：`RLStrategyHost` 自带逐 bar 历史窗口，
        // 跨回测复用会把上一次的窗口带进下一次观测。
        let rl_count = {
            let rl_strategies = kernel_lock(&self.rl_strategies, "rl strategies")?;
            for spec in rl_strategies.iter() {
                let backend =
                    PyRatioBackend::new(spec.predict.clone_ref(py), spec.model_id.clone());
                let host = RLStrategyHost::new(
                    Box::new(backend),
                    spec.spec.clone(),
                    spec.symbol.clone(),
                    spec.action_space.clone(),
                )
                .map_err(|e| {
                    pyo3::exceptions::PyValueError::new_err(format!(
                        "构造 RL host 失败 (model={}): {e}",
                        spec.model_id
                    ))
                })?;
                kernel.add_host(Box::new(host));
            }
            rl_strategies.len()
        };

        let result = kernel.run_history(frames).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("Kernel 回测失败: {e}"))
        })?;
        let py_result =
            PyBacktestResult::from_kernel(&result, (strategies.len() + rl_count) as u32);
        *kernel_lock(&self.last_result, "result")? = Some(py_result.clone());
        Ok(py_result)
    }

    /// 最近一次运行的结果（未跑过返回 `None`）
    fn result(&self) -> PyResult<Option<PyBacktestResult>> {
        Ok(kernel_lock(&self.last_result, "result")?.clone())
    }
}

// ── 崩溃恢复绑定（journal 重演 / 三路对账 / 恢复门禁） ─────────────────

/// journal 头 manifest → Python dict（加载 entries 条目数）
///
/// ReconcileReport / RunManifest 无 pyclass 且 pyo3 未开 serde feature：
/// serde 序列化后经 `json.loads` 还原为 dict，字段形状与 Rust 侧逐一对齐
/// （`resume_guard` 的反序列化走同一 serde 契约，两侧天然一致）。
fn manifest_dict<'py>(
    py: Python<'py>,
    manifest: &RunManifest,
    entries: usize,
) -> PyResult<Bound<'py, PyAny>> {
    let json_mod = py.import("json")?;
    let s = serde_json::to_string(manifest).map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("manifest 序列化失败: {e}"))
    })?;
    let d = json_mod.call_method1("loads", (s,))?;
    d.set_item("entries", entries)?;
    Ok(d)
}

/// 快照 dict 三字段 → (positions, cash, fill_ids)
///
/// `positions` / `cash` / `fill_ids` 缺省取空值：venue 初始无仓、无成交是常态；
/// `LocalSnapshot` 与 `VenueSnapshot` 字段同构，共用本函数。
fn snapshot_from_dict(
    d: &Bound<'_, PyDict>,
) -> PyResult<(BTreeMap<String, f64>, f64, BTreeSet<String>)> {
    let positions: BTreeMap<String, f64> = d
        .get_item("positions")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or_default();
    let cash: f64 = d
        .get_item("cash")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or(0.0);
    let fill_ids: BTreeSet<String> = d
        .get_item("fill_ids")?
        .map(|v| v.extract::<Vec<String>>())
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .collect();
    Ok((positions, cash, fill_ids))
}

/// 崩溃恢复重演器：从 journal 重建 OMS 影子账本并抽取本地快照
///
/// 与 `Kernel`（回测编排，`run_history` 末尾做 finalize 收尾）不同：本类装配
/// `LiveSink::for_replay` 走 `resume_replay`（逐条重演、不做 finalize），恢复出
/// 的是「可继续跑」的内核状态。宿主必须参与重演（journal 行情条目 → 宿主重决策
/// → 订单进 OMS），因此 `resume` 前至少注册一个策略，且决策必须确定（同一输入
/// 同一输出）——这是重演保真成立的前提。
///
/// 与 Rust 侧 crash_recovery 演练同一时序：
///
/// ```python
/// rec = axon_worker.LiveRecovery(initial_cash=100_000.0)
/// rec.add_strategy(strategy)
/// snap = rec.resume("/path/live.journal")   # 构造参数必须与首跑一致
/// report = axon_worker.reconcile(snap, venue_snapshot)
/// axon_worker.resume_guard(report)          # 有 break → ValueError
/// ```
#[pyclass(name = "LiveRecovery", module = "axon_worker")]
pub struct PyLiveRecovery {
    initial_cash: f64,
    /// 重演 sink 的入金（对账现金基线），必须与首跑 LiveSink 入金一致——
    /// 静默改掉只会把账目差异藏起来
    shadow_cash: Decimal,
    instrument_mode: InstrumentMode,
    /// 合约必须 `AccountingMode::Wallet`（开仓不扣现金，cash 口径对账必偏差）
    accounting: AccountingMode,
    interval: String,
    fee_rate: f64,
    strategies: Mutex<Vec<(String, Arc<dyn Strategy>)>>,
    /// 驱动策略 `decide()` 的 runtime；`Kernel` 主回路本身是同步的
    runtime: tokio::runtime::Runtime,
}

#[pymethods]
impl PyLiveRecovery {
    /// `shadow_cash` 缺省取 `initial_cash`（首跑 LiveSink 以 initial_cash 入金）
    #[new]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (initial_cash=100000.0, shadow_cash=None, accounting="cash".to_string(), futures=false, settle=None, contract_size=1.0, interval="1m".to_string(), fee_rate=None))]
    fn new(
        initial_cash: f64,
        shadow_cash: Option<f64>,
        accounting: String,
        futures: bool,
        settle: Option<String>,
        contract_size: f64,
        interval: String,
        fee_rate: Option<f64>,
    ) -> PyResult<Self> {
        if !initial_cash.is_finite() || initial_cash <= 0.0 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "initial_cash 必须是正有限数，收到: {initial_cash}"
            )));
        }
        let shadow_cash = shadow_cash.unwrap_or(initial_cash);
        if !shadow_cash.is_finite() || shadow_cash < 0.0 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "shadow_cash 必须是有限非负数，收到: {shadow_cash}"
            )));
        }
        // `None` = 引擎默认费率；负数 / NaN 会让手续费累计失真，构造期拒绝
        let fee_rate = match fee_rate {
            None => KernelConfig::default().fee_rate,
            Some(rate) if rate.is_finite() && rate >= 0.0 => rate,
            Some(rate) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "fee_rate 必须是有限非负数，收到: {rate}"
                )));
            }
        };
        let accounting_mode = match accounting.as_str() {
            "cash" => AccountingMode::Cash,
            "wallet" => AccountingMode::Wallet,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown accounting: {other} (expected 'cash' / 'wallet')"
                )));
            }
        };
        if futures && accounting_mode == AccountingMode::Cash {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "合约账户必须用 wallet 账务模式（合约开仓不扣现金，cash 口径现金对账必然偏差）",
            ));
        }
        let runtime = tokio::runtime::Runtime::new().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("创建 tokio runtime 失败: {e}"))
        })?;
        Ok(Self {
            initial_cash,
            shadow_cash: Decimal::from_f64(shadow_cash).ok_or_else(|| {
                pyo3::exceptions::PyValueError::new_err("shadow_cash 转 Decimal 失败")
            })?,
            instrument_mode: build_instrument_mode(futures, settle.as_deref(), contract_size)?,
            accounting: accounting_mode,
            interval,
            fee_rate,
            strategies: Mutex::new(Vec::new()),
            runtime,
        })
    }

    /// 注册一个宿主策略（包装规则与 `PyKernel.add_strategy` 完全一致）
    #[pyo3(signature = (strategy, name=None, symbol=None))]
    fn add_strategy(
        &self,
        strategy: &Bound<'_, PyAny>,
        name: Option<String>,
        symbol: Option<String>,
    ) -> PyResult<()> {
        let (symbol, s) = wrap_strategy(strategy, name, symbol)?;
        kernel_lock(&self.strategies, "strategies")?.push((symbol, s));
        Ok(())
    }

    /// 重演 journal（崩溃恢复）并返回本地快照
    ///
    /// 返回 `{positions, cash, fill_ids, manifest}`：前三者是对账的
    /// local 槽（可直接传给 `reconcile`），`manifest` 供归档比对（内含
    /// `entries` 键 = 重演条目数）。manifest 取自 journal 头整份传入，杜绝
    /// Python 侧手工拼
    /// config_hash / data_hash / seed 的错配；`resume_replay` 的逐字段校验是
    /// 第二道门。恢复 kernel 不 attach_journal（会截断既有文件），接管续写由
    /// `resume_replay` 内部的 `Journal::resume` 完成。
    ///
    /// ponytail: 预读（manifest + 条目数）与 `resume_replay` 各解析一遍 journal，
    /// live 规模（万级行文本）可忽略；换内核状态透出 API 不值。
    #[pyo3(signature = (journal_path))]
    fn resume<'py>(&self, py: Python<'py>, journal_path: String) -> PyResult<Bound<'py, PyAny>> {
        let strategies = kernel_lock(&self.strategies, "strategies")?.clone();
        if strategies.is_empty() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "重演需要至少一个宿主策略：journal 行情条目必须经宿主重决策才能恢复 OMS 状态",
            ));
        }
        let path = Path::new(&journal_path);
        // 预读：manifest 对齐 + 条目数；坏行在预读即报错，不带病进重演
        let (manifest, entries) = Journal::open(path).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "读取 journal 失败 {journal_path}: {e}"
            ))
        })?;
        let manifest_d = manifest_dict(py, &manifest, entries.len())?;

        let config = KernelConfig {
            initial_cash: self.initial_cash,
            fee_rate: self.fee_rate,
            instrument_mode: self.instrument_mode,
            interval: self.interval.clone(),
            // 屏障期望品种留空：由 restore_frame_metadata 按 journal 条目重建
            //（传了反而压制重建语义，见 set_expected_if_empty）
            ..KernelConfig::default()
        };
        let sink = LiveSink::for_replay(self.shadow_cash).with_accounting_mode(self.accounting);
        let mut kernel = Kernel::new(config, Box::new(sink));
        kernel.set_manifest(manifest);
        let handle = self.runtime.handle().clone();
        for (symbol, strategy) in &strategies {
            kernel.add_host(Box::new(BarContextSim::new(
                strategy.clone(),
                self.instrument_mode.instrument(symbol),
                handle.clone(),
            )));
        }
        kernel.resume_replay(path).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("journal 重演失败: {e}"))
        })?;

        let live_sink = kernel
            .sink()
            .as_any()
            .downcast_ref::<LiveSink>()
            .ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err(
                    "内核 sink 不是 LiveSink，无法抽取本地快照",
                )
            })?;
        let local = LocalSnapshot::from_sink(live_sink);

        let d = local_snapshot_dict(py, &local)?;
        d.set_item("manifest", manifest_d)?;
        Ok(d.into_any())
    }
}

// ── 实盘驱动器绑定（统一内核实时原语的 Python 面） ─────────────────────

/// `LocalSnapshot` → Python dict `{positions, cash, fill_ids}`
///
/// `positions` / `fill_ids` 键序固定（BTreeMap / BTreeSet），对账两侧可逐键比对。
fn local_snapshot_dict<'py>(
    py: Python<'py>,
    local: &LocalSnapshot,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    let positions = PyDict::new(py);
    for (k, v) in &local.positions {
        positions.set_item(k, v)?;
    }
    d.set_item("positions", positions)?;
    d.set_item("cash", local.cash)?;
    d.set_item("fill_ids", local.fill_ids.iter().collect::<Vec<_>>())?;
    Ok(d)
}

/// 从注册容器组装宿主列表（start / resume 共用）
///
/// RL host 每次调用重建：自带逐 bar 历史窗口，复用会把上一次窗口带进
/// 下一次观测（与 `PyKernel.run` 同口径）。
fn live_hosts(driver: &PyLiveDriver, py: Python<'_>) -> PyResult<Vec<Box<dyn StrategyHost>>> {
    let strategies = kernel_lock(&driver.strategies, "strategies")?.clone();
    host_chain(
        &driver.instrument_mode,
        &strategies,
        &driver.rl_strategies,
        driver.runtime.handle().clone(),
        py,
    )
}

/// hosts 组装的公共主体（PyLiveDriver / PyPaperDriver 同构）
///
/// 规则策略包 `BarContextSim`（逐 bar 累积上下文，由 handle 驱动异步
/// decide()）；RL 规格重建 `RLStrategyHost`。
fn host_chain(
    mode: &InstrumentMode,
    strategies: &[(String, Arc<dyn Strategy>)],
    rl_strategies: &Mutex<Vec<RlStrategySpec>>,
    handle: tokio::runtime::Handle,
    py: Python<'_>,
) -> PyResult<Vec<Box<dyn StrategyHost>>> {
    let mut hosts: Vec<Box<dyn StrategyHost>> = strategies
        .iter()
        .map(|(symbol, s)| {
            Box::new(BarContextSim::new(
                s.clone(),
                mode.instrument(symbol),
                handle.clone(),
            )) as Box<dyn StrategyHost>
        })
        .collect();
    let rl_strategies = kernel_lock(rl_strategies, "rl strategies")?;
    for spec in rl_strategies.iter() {
        let backend = PyRatioBackend::new(spec.predict.clone_ref(py), spec.model_id.clone());
        let host = RLStrategyHost::new(
            Box::new(backend),
            spec.spec.clone(),
            spec.symbol.clone(),
            spec.action_space.clone(),
        )
        .map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "构造 RL host 失败 (model={}): {e}",
                spec.model_id
            ))
        })?;
        hosts.push(Box::new(host));
    }
    Ok(hosts)
}

/// 实盘驱动器：统一内核实时原语的 Python 面
///
/// 装配链：WS 行情 fan-out → KernelPump → Kernel（实时步进）→ LiveSink
/// （下单与回报簿记）。数据是拉取式：每 `tick()` 尾部拉一次 WS 消息，
/// 回报账面落定落后一拍（two-tick 语义）。
///
/// v1 边界（`LiveDriver::new` 构造期强制）：仅永续合约（Swap）+ 钱包口径
/// （Wallet）。adapter 由内部自建（收 `ExchangeConfig`），连接生命周期归
/// driver（connect 成败皆可重试）。崩溃恢复走 `resume`：journal 重演 →
/// 三路对账 → 守卫放行；纯重演子集（无交易所）见 `LiveRecovery`。
#[pyclass(name = "LiveDriver", module = "axon_worker")]
pub struct PyLiveDriver {
    /// 装配原料（add_rl_strategy 品种解析 + hosts 组装共用）
    instrument_mode: InstrumentMode,
    /// 规则策略容器（复刻 PyKernel：攒列表、start/resume 时构造 hosts）
    strategies: Mutex<Vec<(String, Arc<dyn Strategy>)>>,
    rl_strategies: Mutex<Vec<RlStrategySpec>>,
    /// 核心 driver；构造期即建成（adapter 封装 + v1 校验在 new 里发生）
    inner: Mutex<LiveDriver>,
    /// 驱动策略 decide() 的 runtime；Kernel 主回路本身是同步的
    runtime: tokio::runtime::Runtime,
}

#[pymethods]
impl PyLiveDriver {
    /// adapter 裸建后封进 tokio Mutex 的 trait object：gateway（自建 runtime）
    /// 接管其全部异步调用，绑定层不再直接触碰 adapter
    #[allow(clippy::too_many_arguments)]
    #[new]
    #[pyo3(signature = (config, symbols, initial_cash=100000.0, shadow_cash=None, accounting="wallet".to_string(), futures=true, settle=None, contract_size=1.0, interval="1m".to_string(), fee_rate=None))]
    fn new(
        config: PyExchangeConfig,
        symbols: Vec<String>,
        initial_cash: f64,
        shadow_cash: Option<f64>,
        accounting: String,
        futures: bool,
        settle: Option<String>,
        contract_size: f64,
        interval: String,
        fee_rate: Option<f64>,
    ) -> PyResult<Self> {
        if !initial_cash.is_finite() || initial_cash <= 0.0 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "initial_cash 必须是正有限数，收到: {initial_cash}"
            )));
        }
        let shadow_cash = shadow_cash.unwrap_or(initial_cash);
        if !shadow_cash.is_finite() || shadow_cash < 0.0 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "shadow_cash 必须是有限非负数，收到: {shadow_cash}"
            )));
        }
        // `None` = 引擎默认费率；负数 / NaN 会让手续费累计失真，构造期拒绝
        let fee_rate = match fee_rate {
            None => KernelConfig::default().fee_rate,
            Some(rate) if rate.is_finite() && rate >= 0.0 => rate,
            Some(rate) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "fee_rate 必须是有限非负数，收到: {rate}"
                )));
            }
        };
        let accounting_mode = match accounting.as_str() {
            "cash" => AccountingMode::Cash,
            "wallet" => AccountingMode::Wallet,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown accounting: {other} (expected 'cash' / 'wallet')"
                )));
            }
        };
        let instrument_mode = build_instrument_mode(futures, settle.as_deref(), contract_size)?;
        let kernel_config = KernelConfig {
            initial_cash,
            fee_rate,
            instrument_mode,
            interval: interval.clone(),
            ..KernelConfig::default()
        };
        let adapter: Arc<tokio::sync::Mutex<Box<dyn ExchangeAdapter>>> = Arc::new(
            tokio::sync::Mutex::new(Box::new(BinanceAdapter::new(config.inner))),
        );
        // LiveDriver::new 会校验 v1 边界（Spot / Cash / 空品种在此报错），不放行坏配置
        let live = LiveDriver::new(
            adapter,
            kernel_config,
            accounting_mode,
            shadow_cash,
            symbols,
        )
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
        Ok(Self {
            instrument_mode,
            strategies: Mutex::new(Vec::new()),
            rl_strategies: Mutex::new(Vec::new()),
            inner: Mutex::new(live),
            runtime: tokio::runtime::Runtime::new().map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("创建 tokio runtime 失败: {e}"))
            })?,
        })
    }

    /// 注册一个宿主策略（包装规则与 `Kernel.add_strategy` 完全一致）
    #[pyo3(signature = (strategy, name=None, symbol=None))]
    fn add_strategy(
        &self,
        strategy: &Bound<'_, PyAny>,
        name: Option<String>,
        symbol: Option<String>,
    ) -> PyResult<()> {
        let (symbol, s) = wrap_strategy(strategy, name, symbol)?;
        kernel_lock(&self.strategies, "strategies")?.push((symbol, s));
        Ok(())
    }

    /// 注册 RL 策略（校验链统一在 `build_rl_spec`，与 `Kernel.add_rl_strategy` 同口径）
    ///
    /// 文档同 `Kernel.add_rl_strategy`：`predict` 是 `(list[float]) -> float`
    /// 的目标占比回调；`stats` 是训练期归一化统计量，条数必须等于特征数。
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (predict, model_id, spec_version, window_size, features, action_space=None, symbol="BTC/USDT", stats=None))]
    fn add_rl_strategy(
        &self,
        predict: &Bound<'_, PyAny>,
        model_id: &str,
        spec_version: &str,
        window_size: usize,
        features: &Bound<'_, PyAny>,
        action_space: Option<&Bound<'_, PyDict>>,
        symbol: &str,
        stats: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let spec = build_rl_spec(
            predict,
            model_id,
            spec_version,
            window_size,
            features,
            action_space,
            symbol,
            &self.instrument_mode,
            stats,
        )?;
        kernel_lock(&self.rl_strategies, "rl strategies")?.push(spec);
        Ok(())
    }

    fn connect(&mut self) -> PyResult<()> {
        kernel_lock(&self.inner, "live driver")?
            .connect()
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)
    }

    /// 全新起跑：建内核、挂 journal、注册 hosts（需先 connect 成功）
    ///
    /// journal 是崩溃恢复的根基，路径必须是文件（目录会报错 fail-fast）。
    #[pyo3(signature = (journal_path))]
    fn start(&self, py: Python<'_>, journal_path: String) -> PyResult<()> {
        let hosts = live_hosts(self, py)?;
        kernel_lock(&self.inner, "live driver")?
            .start(Path::new(&journal_path), hosts)
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)
    }

    /// 崩溃恢复：journal 重演 → 本地快照 → 交易所对账 → 守卫放行
    ///
    /// 守卫拒绝（对账超容差 / fill 断裂）时报错且网关放回 driver 可整体重试；
    /// 成功返回 ReconcileReport dict（serde → `json.loads`，与 `reconcile`
    /// 的返回契约一致）。
    #[pyo3(signature = (journal_path))]
    fn resume<'py>(&self, py: Python<'py>, journal_path: String) -> PyResult<Bound<'py, PyAny>> {
        let hosts = live_hosts(self, py)?;
        let report = kernel_lock(&self.inner, "live driver")?
            .resume(Path::new(&journal_path), hosts)
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        let json_mod = py.import("json")?;
        let s = serde_json::to_string(&report).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("report 序列化失败: {e}"))
        })?;
        json_mod.call_method1("loads", (s,))
    }

    /// 推进一拍：泵行情 → 步进内核 → 尾部拉 WS 回报（下一拍消化）
    fn tick(&mut self) -> PyResult<()> {
        kernel_lock(&self.inner, "live driver")?
            .tick()
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)
    }

    /// 本地账面快照：`{positions, cash, fill_ids}`（需已 start/resume）
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let snap = kernel_lock(&self.inner, "live driver")?
            .snapshot()
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        local_snapshot_dict(py, &snap)
    }

    /// 收尾：请求 sink 终止（.flush 状态下的持仓/订单收口由内核编排决定）
    fn shutdown(&mut self) -> PyResult<()> {
        kernel_lock(&self.inner, "live driver")?
            .shutdown()
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)
    }
}

/// 纸面驱动器：PaperDriver 的 Python 面（模拟盘，纯内存账本）
///
/// 装配链对齐 [`PyLiveDriver`]（同一的 `host_chain` 组装），执行换成
/// `PaperSink` 虚拟撮合：不下真实单、无 journal / resume，账面重启即重置
/// （对齐 QuantCell paper 语义）。行情走 [`PyPaperDriver::feed_bar`] 手工灌
/// 收市 bar（QuantCell 拉自有行情服务后逐根灌入）；接 testnet WS 的
/// gateway 模式只在 Rust 侧开放，需要时再把 adapter 参数补进 `new`。
#[pyclass(name = "PaperDriver", module = "axon_worker")]
pub struct PyPaperDriver {
    /// 装配原料（add_rl_strategy 品种解析 + hosts 组装共用）
    instrument_mode: InstrumentMode,
    /// 规则策略容器（与 PyLiveDriver 同构：攒列表、start 时构造 hosts）
    strategies: Mutex<Vec<(String, Arc<dyn Strategy>)>>,
    rl_strategies: Mutex<Vec<RlStrategySpec>>,
    /// 核心 driver；构造期即建成（feed_bar / tick / snapshot 要求已 start）
    inner: Mutex<PaperDriver>,
    /// 驱动策略 decide() 的 runtime；内核主回路本身是同步的
    runtime: tokio::runtime::Runtime,
}

#[pymethods]
impl PyPaperDriver {
    /// 纯内存撮合不接交易所，`new` 无 exchange config；默认模拟盘口
    /// （`SeedLiquidity` half_spread=0.0005）保证市价单有对手价可成交
    #[new]
    #[pyo3(signature = (symbols, initial_cash=100000.0, futures=true, settle=None, contract_size=1.0, interval="1m".to_string(), fee_rate=None))]
    fn new(
        symbols: Vec<String>,
        initial_cash: f64,
        futures: bool,
        settle: Option<String>,
        contract_size: f64,
        interval: String,
        fee_rate: Option<f64>,
    ) -> PyResult<Self> {
        if !initial_cash.is_finite() || initial_cash <= 0.0 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "initial_cash 必须是正有限数，收到: {initial_cash}"
            )));
        }
        // `None` = 引擎默认费率；负数 / NaN 会让手续费累计失真，构造期拒绝
        let fee_rate = match fee_rate {
            None => KernelConfig::default().fee_rate,
            Some(rate) if rate.is_finite() && rate >= 0.0 => rate,
            Some(rate) => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "fee_rate 必须是有限非负数，收到: {rate}"
                )));
            }
        };
        let instrument_mode = build_instrument_mode(futures, settle.as_deref(), contract_size)?;
        let config = KernelConfig {
            initial_cash,
            fee_rate,
            instrument_mode,
            interval: interval.clone(),
            ..KernelConfig::default()
        };
        // 空 symbols 由 PaperDriver::new 校验拒绝（与 LiveDriver 同文案）
        let paper = PaperDriver::new(None, config, symbols)
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        Ok(Self {
            instrument_mode,
            strategies: Mutex::new(Vec::new()),
            rl_strategies: Mutex::new(Vec::new()),
            inner: Mutex::new(paper),
            runtime: tokio::runtime::Runtime::new().map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("创建 tokio runtime 失败: {e}"))
            })?,
        })
    }

    /// 注册一个宿主策略（包装规则与 `Kernel.add_strategy` 完全一致）
    #[pyo3(signature = (strategy, name=None, symbol=None))]
    fn add_strategy(
        &self,
        strategy: &Bound<'_, PyAny>,
        name: Option<String>,
        symbol: Option<String>,
    ) -> PyResult<()> {
        let (symbol, s) = wrap_strategy(strategy, name, symbol)?;
        kernel_lock(&self.strategies, "strategies")?.push((symbol, s));
        Ok(())
    }

    /// 注册 RL 策略（校验链统一在 `build_rl_spec`，与 `LiveDriver` 同口径）
    ///
    /// `predict` 是 `(list[float]) -> float` 的目标权益占比回调；`stats` 是
    /// 训练期归一化统计量，条数必须等于特征数。
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (predict, model_id, spec_version, window_size, features, action_space=None, symbol="BTC/USDT", stats=None))]
    fn add_rl_strategy(
        &self,
        predict: &Bound<'_, PyAny>,
        model_id: &str,
        spec_version: &str,
        window_size: usize,
        features: &Bound<'_, PyAny>,
        action_space: Option<&Bound<'_, PyDict>>,
        symbol: &str,
        stats: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let spec = build_rl_spec(
            predict,
            model_id,
            spec_version,
            window_size,
            features,
            action_space,
            symbol,
            &self.instrument_mode,
            stats,
        )?;
        kernel_lock(&self.rl_strategies, "rl strategies")?.push(spec);
        Ok(())
    }

    /// 起跑：建内核（PaperSink 虚拟撮合）、注册 hosts（无外部连接，直接 start）
    fn start(&self, py: Python<'_>) -> PyResult<()> {
        let strategies = kernel_lock(&self.strategies, "strategies")?.clone();
        let hosts = host_chain(
            &self.instrument_mode,
            &strategies,
            &self.rl_strategies,
            self.runtime.handle().clone(),
            py,
        )?;
        kernel_lock(&self.inner, "paper driver")?
            .start(hosts)
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)
    }

    /// 手工灌一根收市 bar（feed_bar 驱动模式）
    ///
    /// `timestamp` 为 bar 开市时刻（纳秒），事件时间戳 = 收市时刻（防前视）；
    /// `symbol` 必须是 `new` 时声明过的品种。调用后需 `tick()` 步进消化。
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (symbol, timestamp, open, high, low, close, volume))]
    fn feed_bar(
        &mut self,
        symbol: &str,
        timestamp: i64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
    ) -> PyResult<()> {
        kernel_lock(&self.inner, "paper driver")?
            .feed_bar(symbol, timestamp, open, high, low, close, volume)
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)
    }

    /// 推进一拍：步进消化已灌事件（多品种时栅栏未齐则空转等待）
    fn tick(&mut self) -> PyResult<()> {
        kernel_lock(&self.inner, "paper driver")?
            .tick()
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)
    }

    /// 运行中账面快照：`{equity, positions}`
    ///
    /// equity = 现金 + 持仓按最近价盯市；positions key = `BASE/QUOTE`、
    /// 值 = 净持仓数量（非零仓才出现在 dict 里）。
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let snap = kernel_lock(&self.inner, "paper driver")?
            .snapshot()
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        let dict = PyDict::new(py);
        dict.set_item("equity", snap.equity)?;
        let positions = PyDict::new(py);
        for (key, qty) in &snap.positions {
            positions.set_item(key, qty)?;
        }
        dict.set_item("positions", positions)?;
        Ok(dict)
    }

    /// 收尾：paper 账纯内存、无外部挂起资源，幂等 no-op（保持接口对称）
    fn shutdown(&mut self) -> PyResult<()> {
        kernel_lock(&self.inner, "paper driver")?
            .shutdown()
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)
    }
}

/// 只读探查 journal 头 manifest（不接管文件、不重演）
///
/// 供恢复前预检（journal 是否可读 / 归档 manifest 指纹）；任何坏行直接报错，
/// 与 `Journal::open` 的全量严格口径一致。
#[pyfunction]
#[pyo3(signature = (path))]
fn read_manifest(py: Python<'_>, path: String) -> PyResult<Bound<'_, PyAny>> {
    let (manifest, entries) = Journal::open(Path::new(&path)).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("读取 journal 失败 {path}: {e}"))
    })?;
    manifest_dict(py, &manifest, entries.len())
}

/// 三路对账：`local`（`resume()` 返回的快照）vs `venue`（交易所流水组装）
///
/// 只报告不修账。`position_tolerance` / `cash_tolerance` 缺省走引擎默认
///（1e-8 / 1e-6）；负容差会把「差异必报」退化成「永不报」，构造期拒绝。
/// `local` 可直接传 `resume()` 的返回值（多出的 entries / manifest 键被忽略）。
#[pyfunction]
#[pyo3(name = "reconcile", signature = (local, venue, position_tolerance=None, cash_tolerance=None))]
fn reconcile_snapshots<'py>(
    py: Python<'py>,
    local: &Bound<'_, PyAny>,
    venue: &Bound<'_, PyAny>,
    position_tolerance: Option<f64>,
    cash_tolerance: Option<f64>,
) -> PyResult<Bound<'py, PyAny>> {
    let mut config = ReconcileConfig::default();
    for (name, value, target) in [
        (
            "position_tolerance",
            position_tolerance,
            &mut config.position_tolerance,
        ),
        ("cash_tolerance", cash_tolerance, &mut config.cash_tolerance),
    ] {
        let Some(t) = value else { continue };
        if !t.is_finite() || t < 0.0 {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "{name} 必须是有限非负数，收到: {t}"
            )));
        }
        *target = t;
    }
    let (positions, cash, fill_ids) = snapshot_from_dict(local.cast::<PyDict>()?)?;
    let local = LocalSnapshot {
        positions,
        cash,
        fill_ids,
    };
    let (positions, cash, fill_ids) = snapshot_from_dict(venue.cast::<PyDict>()?)?;
    let venue = VenueSnapshot {
        positions,
        cash,
        fill_ids,
    };
    let report = reconcile_impl(&local, &venue, &config);
    let json_mod = py.import("json")?;
    let s = serde_json::to_string(&report).map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("report 序列化失败: {e}"))
    })?;
    json_mod.call_method1("loads", (s,))
}

/// 恢复门禁（Rust 侧 `resume_guard` 语义原样透传，不在 Python 侧重写）
///
/// 持仓 break 或现金 break → `ValueError` 禁止自动续跑；仅成交差异 → 放行，
/// 但调用方必须告警人工核对（交易所侧在崩溃窗口内可能已发生 journal 之外的
/// 状态变化）。`report` 形状以 serde 契约为准（`reconcile()` 的返回值可直接
/// 传入）。
#[pyfunction]
#[pyo3(name = "resume_guard", signature = (report))]
fn enforce_resume_guard(py: Python<'_>, report: &Bound<'_, PyAny>) -> PyResult<()> {
    let json_mod = py.import("json")?;
    let value = dict_to_json_value(&json_mod, report)?;
    let parsed: ReconcileReport = serde_json::from_value(value).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "report 不符合 ReconcileReport serde 契约: {e}"
        ))
    })?;
    resume_guard_impl(&parsed)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("恢复门禁: {e}")))
}

/// `Kernel` 的可变状态加锁（统一错误信息，避免每处复制 map_err）
fn kernel_lock<'a, T>(m: &'a Mutex<T>, what: &str) -> PyResult<MutexGuard<'a, T>> {
    m.lock().map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("Kernel {what} lock 失败: {e}"))
    })
}

/// futures / settle / contract_size → 品种撮合口径
fn build_instrument_mode(
    futures: bool,
    settle: Option<&str>,
    contract_size: f64,
) -> PyResult<InstrumentMode> {
    if !futures {
        return Ok(InstrumentMode::Spot);
    }
    Ok(InstrumentMode::Swap {
        settle: parse_settle(settle)?,
        contract_size,
    })
}

/// 结算方式字符串 → `SwapSettle`（缺省 `usd_margin`）
fn parse_settle(settle: Option<&str>) -> PyResult<SwapSettle> {
    match settle.unwrap_or("usd_margin") {
        "usd_margin" => Ok(SwapSettle::UsdMargin),
        "coin_margin" => Ok(SwapSettle::CoinMargin),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown settle: {other} (expected 'usd_margin' / 'coin_margin')"
        ))),
    }
}

/// `dict[str, list[dict | None]]` → 逐网格点 bar 列
fn parse_bars(bars_by_symbol: &Bound<'_, PyDict>) -> PyResult<BTreeMap<String, Vec<Option<Bar>>>> {
    let mut bars_map: BTreeMap<String, Vec<Option<Bar>>> = BTreeMap::new();
    for (k, v) in bars_by_symbol.iter() {
        let key: String = k.extract()?;
        let items: Vec<Bound<'_, PyAny>> = v.extract()?;
        let mut per_grid: Vec<Option<Bar>> = Vec::with_capacity(items.len());
        for it in items.iter() {
            if it.is_none() {
                per_grid.push(None);
            } else {
                per_grid.push(Some(dict_to_bar(it.cast::<PyDict>()?)?));
            }
        }
        bars_map.insert(key, per_grid);
    }
    Ok(bars_map)
}

/// `states_by_symbol` → 逐 bar state 列
///
/// 值为 list 时按逐 bar 注入；值为单对象时展开成 `grid_len` 份「每 bar 同值」。
fn parse_states(
    states_by_symbol: Option<&Bound<'_, PyDict>>,
    grid_len: usize,
) -> PyResult<BTreeMap<String, Vec<serde_json::Value>>> {
    let Some(d) = states_by_symbol else {
        return Ok(BTreeMap::new());
    };
    let py = d.py();
    let json_mod = py.import("json")?;
    let mut m = BTreeMap::new();
    for (k, v) in d.iter() {
        let key: String = k.extract()?;
        let list = if v.is_instance_of::<PyList>() {
            let items: Vec<Bound<'_, PyAny>> = v.extract()?;
            items
                .iter()
                .map(|it| dict_to_json_value(&json_mod, it))
                .collect::<PyResult<Vec<_>>>()?
        } else {
            vec![dict_to_json_value(&json_mod, &v)?; grid_len]
        };
        m.insert(key, list);
    }
    Ok(m)
}

/// 资金费查找表：`symbol -> (纳秒时间戳 -> (rate, mark_price))`
type FundingTable = BTreeMap<String, BTreeMap<i64, (f64, f64)>>;

/// `funding_by_symbol` → 资金费查找表（键为 bar 纳秒时间戳）
fn parse_funding(funding_by_symbol: Option<&Bound<'_, PyDict>>) -> PyResult<FundingTable> {
    let Some(d) = funding_by_symbol else {
        return Ok(BTreeMap::new());
    };
    let mut m = BTreeMap::new();
    for (k, v) in d.iter() {
        let key: String = k.extract()?;
        let sched: BTreeMap<i64, (f64, f64)> = v.extract()?;
        m.insert(key, sched);
    }
    Ok(m)
}

/// Python dict → 每品种保证金规格
fn parse_margin_specs(specs: &Bound<'_, PyDict>) -> PyResult<HashMap<String, MarginSpec>> {
    let mut parsed: HashMap<String, MarginSpec> = HashMap::new();
    for (k, v) in specs.iter() {
        let sym: String = k.extract()?;
        let d = v.cast::<PyDict>()?;

        let leverage: f64 = d
            .get_item("leverage")?
            .map(|x| x.extract::<f64>())
            .transpose()?
            .unwrap_or(1.0);
        let mode = match d
            .get_item("mode")?
            .map(|x| x.extract::<String>())
            .transpose()?
            .unwrap_or_else(|| "cross".to_string())
            .as_str()
        {
            "isolated" => MarginMode::Isolated,
            "cross" => MarginMode::Cross,
            other => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "unknown margin mode: {other} (expected 'isolated' / 'cross')"
                )));
            }
        };

        // brackets 没有默认值：缺档位就无法算维持保证金，宁可直接报错
        let brackets_obj = d.get_item("brackets")?.ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!("{sym} 缺少 brackets"))
        })?;
        let brackets_list: Vec<Bound<'_, PyAny>> = brackets_obj.extract()?;
        let mut brackets = Vec::with_capacity(brackets_list.len());
        for item in brackets_list {
            let bd = item.cast::<PyDict>()?;
            brackets.push(Bracket {
                notional_floor: bd
                    .get_item("notional_floor")?
                    .map(|x| x.extract::<f64>())
                    .transpose()?
                    .unwrap_or(0.0),
                notional_cap: bd
                    .get_item("notional_cap")?
                    .map(|x| x.extract::<Option<f64>>())
                    .transpose()?
                    .flatten(),
                mmr: bd
                    .get_item("mmr")?
                    .map(|x| x.extract::<f64>())
                    .transpose()?
                    .unwrap_or(0.0),
                cum: bd
                    .get_item("cum")?
                    .map(|x| x.extract::<f64>())
                    .transpose()?
                    .unwrap_or(0.0),
                liquidation_fee_rate: bd
                    .get_item("liquidation_fee_rate")?
                    .map(|x| x.extract::<f64>())
                    .transpose()?
                    .unwrap_or(0.001),
                max_leverage: bd
                    .get_item("max_leverage")?
                    .map(|x| x.extract::<f64>())
                    .transpose()?
                    .unwrap_or(125.0),
            });
        }
        // 档位来源只用于结果回显，缺省按 "default"（旧调用方不传该键）
        let bracket_source = d
            .get_item("bracket_source")?
            .map(|x| x.extract::<String>())
            .transpose()?
            .unwrap_or_else(|| "default".to_string());
        parsed.insert(
            sym,
            MarginSpec {
                leverage,
                mode,
                brackets,
                bracket_source,
            },
        );
    }
    Ok(parsed)
}

/// 把 Python 策略对象包装成 `Arc<dyn Strategy>`，同时返回它声明的交易 symbol
///
/// - `strategy.RuleStrategyImpl`：直接取内部 `Arc`，symbol 从策略自身读
/// - 任意 Python 对象：包装成 `PythonStrategyImpl`，symbol 优先取显式参数
fn wrap_strategy(
    strategy: &Bound<'_, PyAny>,
    name: Option<String>,
    symbol: Option<String>,
) -> PyResult<(String, Arc<dyn Strategy>)> {
    if let Ok(py_rule) = strategy.extract::<PyRef<'_, PyRuleStrategyImpl>>() {
        let inner = py_rule.inner.clone();
        let symbol = inner.symbol().to_string();
        return Ok((symbol, inner));
    }

    let name = name.unwrap_or_else(|| {
        strategy
            .getattr("name")
            .and_then(|v| v.extract::<String>())
            .unwrap_or_else(|_| "python_strategy".to_string())
    });
    let symbol = symbol.unwrap_or_else(|| {
        strategy
            .getattr("symbol")
            .and_then(|v| v.extract::<String>())
            .unwrap_or_else(|_| "UNKNOWN".to_string())
    });
    Ok((
        symbol.clone(),
        Arc::new(PythonStrategyImpl::new(
            name,
            symbol,
            strategy.clone().unbind(),
        )),
    ))
}

/// Python 对象 → serde_json::Value（经 json.dumps，避免 str() 的单引号问题）
fn dict_to_json_value(
    json_mod: &Bound<'_, PyAny>,
    v: &Bound<'_, PyAny>,
) -> PyResult<serde_json::Value> {
    let json_str: String = json_mod.call_method1("dumps", (v,))?.extract()?;
    Ok(serde_json::from_str(&json_str).unwrap_or(serde_json::Value::Null))
}

/// 从 Python dict 构造 Bar
fn dict_to_bar(d: &Bound<'_, PyDict>) -> PyResult<Bar> {
    let ts_nanos: i64 = d
        .get_item("timestamp")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("bar 缺少 timestamp"))?
        .extract()?;
    let open: f64 = d
        .get_item("open")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("bar 缺少 open"))?
        .extract()?;
    let high: f64 = d
        .get_item("high")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("bar 缺少 high"))?
        .extract()?;
    let low: f64 = d
        .get_item("low")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("bar 缺少 low"))?
        .extract()?;
    let close: f64 = d
        .get_item("close")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("bar 缺少 close"))?
        .extract()?;
    let volume: f64 = d
        .get_item("volume")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("bar 缺少 volume"))?
        .extract()?;

    Ok(Bar {
        timestamp: Timestamp { nanos: ts_nanos },
        open: Price::from_f64(open),
        high: Price::from_f64(high),
        low: Price::from_f64(low),
        close: Price::from_f64(close),
        volume: Quantity::from_f64(volume),
    })
}

/// 在给定 PyModule 上注册 axon_worker 的 Python 绑定
pub fn register_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyBacktestResult>()?;
    m.add_class::<PyBacktestRunner>()?;
    m.add_class::<PyKernel>()?;
    m.add_class::<PyLiveRecovery>()?;
    m.add_class::<PyLiveDriver>()?;
    m.add_class::<PyPaperDriver>()?;
    m.add_function(wrap_pyfunction!(read_manifest, m)?)?;
    m.add_function(wrap_pyfunction!(reconcile_snapshots, m)?)?;
    m.add_function(wrap_pyfunction!(enforce_resume_guard, m)?)?;
    Ok(())
}
