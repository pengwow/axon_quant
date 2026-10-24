//! axon-worker Python 绑定
//!
//! 暴露：
//! - `BacktestRunner`：Rust 原生回测编排器（构造 + 注册策略 + run）
//! - `Kernel`：统一交易内核的薄包装（M2），结果与 `BacktestRunner` 等价
//! - `BacktestResult`：回测结果 DTO
//!
//! `run()` 内部用 tokio runtime `block_on` 执行 async 回测循环，
//! bars 从 Python `list[dict]` 构造（每根 bar 含 timestamp/open/high/low/close/volume）。

#![cfg(feature = "python")]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};

use axon_backtest::engine::EffectiveMarginSpec;
use axon_backtest::margin::{Bracket, LiquidateReason, LiquidationEvent, MarginMode, MarginSpec};
use axon_core::market::bar::Bar;
use axon_core::portfolio::{FillRecord, TradeRecord};
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Price, Quantity, SwapSettle};
use axon_kernel::{
    BacktestSink, InferenceBackend, InstrumentMode, Kernel, KernelConfig, KernelResult,
    RLStrategyHost, build_frames,
};
use axon_rl::{
    Action, ActionSpace, ContinuousActionSpace, FeatureConfig, ObservationSpec, RunningStats,
};
use axon_strategy::impl_::PythonStrategyImpl;
use axon_strategy::python::PyRuleStrategyImpl;
use axon_strategy::trait_::Strategy;

use crate::backtest::{BacktestResult, BacktestRunner};
use crate::kernel_host::BarContextSim;

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

        kernel_lock(&self.rl_strategies, "rl strategies")?.push(RlStrategySpec {
            predict: predict.clone().unbind(),
            model_id: model_id.to_string(),
            spec,
            symbol: self.instrument_mode.instrument(symbol),
            action_space,
        });
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
    #[pyo3(signature = (bars_by_symbol, states_by_symbol=None, funding_by_symbol=None, capital_budget=None))]
    fn run(
        &self,
        py: Python<'_>,
        bars_by_symbol: &Bound<'_, PyDict>,
        states_by_symbol: Option<Bound<'_, PyDict>>,
        funding_by_symbol: Option<Bound<'_, PyDict>>,
        capital_budget: Option<f64>,
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
    Ok(())
}
