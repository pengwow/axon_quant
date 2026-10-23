//! M3 Shadow oracle：同一份历史数据分别喂给**旧路径**与**新内核**，按类别计数
//! 差异并落盘归档。
//!
//! - **旧路径（语义真源）**：`BacktestRunner::run()`，策略自报**绝对数量**
//!   `target_position`。
//! - **新路径**：`Kernel::run_history()`，Host 吐 `OrderIntent::SetTarget`
//!   （**权益占比**），由 `BacktestSink` 在唯一换算点转成绝对数量。
//!
//! # 为什么不是「逐事件 9 字段 diff」
//!
//! 设计文档（`QuantCell/docs/superpowers/specs/2026-10-23-unified-kernel-design.md`
//! §10）给出的规范形式是逐事件比较
//! `(event_kind, timestamp, instrument, price, quantity, side, order_seq,
//! position_after, nav_after)`。但旧路径 `BacktestRunner::run()` **只返回聚合后的
//! [`BacktestResult`]，不暴露逐事件流**（既没有事件回调，也没有 journal 落盘，
//! 引擎的 dispatch 也无法从外部挂钩）。M3 因此只能退化为「按类别分组比对」：
//!
//! | 类别 | 旧路径字段 | 新内核字段 |
//! |------|-----------|-----------|
//! | `nav_curve` | `bar_nav_curve` | `run.bar_nav_curve` |
//! | `balance_curve` | `bar_balance_curve` | `run.bar_balance_curve` |
//! | `fills` | `fills_detail` | `run.fills_detail` |
//! | `trades` | `trades` | `run.trades` |
//! | `positions` | `final_positions` | `run.positions` |
//! | `totals` | 9 个标量 | 9 个标量 |
//!
//! 这是**已知天花板**，不是刻意简化：逐事件流拿不到，就只能比聚合产物。
//! `equity_curve`（稀疏、事件触发采样，`RunResult` 里已标 DEPRECATED）不在上面的
//! 类别里：它只是同一份账务的另一种切片，密集的 `bar_nav_curve` /
//! `bar_balance_curve` 已覆盖同一信息，且 `kernel_parity` 的逐字段 parity 已单独
//! 比对过它。
//!
//! **升级路径**：给 `BacktestEngine` 加事件钩子（或直接复用内核 `Journal`），
//! 把 dispatch 的每条事件吐出来，`fills` / `trades` 两类即可升级成逐事件 diff；
//! `run_shadow` 的签名与 [`ShadowReport`](crate::shadow::ShadowReport) 结构不用变，
//! 只需新增类别。
//!
//! # 归一化
//!
//! - 浮点比较用 `1e-9` 相对误差（绝对值为 0 时退化为 `1e-12` 绝对误差）；
//! - `TradeRecord` 的定点字段（×1e6）允许 1~2 微单位误差：`qty * price / equity`
//!   的反推往返在 f64 末位可能差 1 ulp，量化到 ×1e6 后就是 1~2 个整数单位；
//! - order id / fill id **不比较**：两条路径的 id 计数器分段不同（设计文档要求
//!   「归一化 `order_id` 分段偏移」），比较它们在语义上没有意义。这也是
//!   多品种场景必须让「同一帧最多一个品种调仓」的原因 —— 否则引擎 `HashMap`
//!   迭代序会让 id 到品种的分配漂移，连成交顺序都可能变。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use axon_backtest::engine::RunResult;
use axon_core::Timestamp;
use axon_core::market::bar::Bar;
use axon_core::portfolio::{FillRecord, TradeRecord};
use axon_core::types::Instrument;
use axon_kernel::{Kernel, build_frames};
use serde::Serialize;

use crate::{BacktestResult, BacktestRunner};

/// f64 相对误差上限
const REL_TOL: f64 = 1e-9;
/// f64 绝对误差下限（两侧都接近 0 时用）
const ABS_TOL: f64 = 1e-12;
/// 每类差异保留的摘要条数上限
const MAX_SAMPLES: usize = 5;
/// 定点（×1e6）账务字段的容差（见模块文档的归一化说明）
const MICRO_TOL: i64 = 2;

/// Shadow 双跑的外部输入
///
/// 字段与 `BacktestRunner::run()` 的四个入参一一对应。注意
/// `funding_by_symbol` / `capital_budget` 只有**旧路径**从这里读：新内核的
/// 配置在构造 [`Kernel`] 时固化（`KernelConfig`），调用方必须给两套装配传
/// **同一份**值，否则差异会被如实计入报告而不是被这里悄悄抹平。
#[derive(Debug, Clone, Default)]
pub struct ShadowInput {
    /// 每个 symbol 的等长逐网格 bar 列表（`None` = 该网格点无真实 bar）
    pub bars_by_symbol: BTreeMap<String, Vec<Option<Bar>>>,
    /// 逐 bar state（下标与 bars 对齐；缺项按 `Value::Null` 处理）
    pub states_by_symbol: BTreeMap<String, Vec<serde_json::Value>>,
    /// 资金费查找表（`symbol -> (纳秒时间戳 -> (rate, mark))`）
    pub funding_by_symbol: BTreeMap<String, BTreeMap<i64, (f64, f64)>>,
    /// 组合资金预算（调用方传 `initial_cash × leverage`）
    pub capital_budget: Option<f64>,
}

/// 单类差异
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShadowDiff {
    /// 类别名：`nav_curve` / `balance_curve` / `fills` / `trades` / `positions`
    /// / `totals`（以及装配失败时的 `run_error`）
    pub category: String,
    /// 该类差异条数（`0` 表示这一类比pair 一致）
    pub count: usize,
    /// 前 `MAX_SAMPLES` 条差异摘要
    pub samples: Vec<String>,
}

/// 一次双跑的差异报告（落盘归档的唯一产物）
///
/// `matched == true` 当且仅当所有类别 `count == 0`；任何类别非零即失败，
/// 不区分「看起来不重要」的类别。
#[derive(Debug, Clone, Serialize)]
pub struct ShadowReport {
    /// 场景名（也是落盘文件名，见 [`Self::write_json`]）
    pub scenario: String,
    /// 差异是否为 0
    pub matched: bool,
    /// 按类别逐条计数（顺序固定）
    pub diffs: Vec<ShadowDiff>,
    /// 旧路径关键指标一行摘要
    pub old_summary: String,
    /// 新内核关键指标一行摘要
    pub new_summary: String,
}

impl ShadowReport {
    /// 落盘为 `<dir>/<scenario>.json`（`dir` 不存在时自动创建），返回文件路径
    pub fn write_json(&self, dir: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", self.scenario));
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::other(format!("序列化 shadow 报告失败: {e}")))?;
        std::fs::write(&path, json)?;
        Ok(path)
    }

    /// 装配阶段就失败（帧构造 / 内核报错）时的报告：记为一条 `run_error` 差异
    fn failed(scenario: &str, old_summary: String, error: String) -> Self {
        Self {
            scenario: scenario.to_string(),
            matched: false,
            diffs: vec![ShadowDiff {
                category: "run_error".to_string(),
                count: 1,
                samples: vec![error.clone()],
            }],
            old_summary,
            new_summary: format!("error: {error}"),
        }
    }
}

/// 同一份输入跑完旧路径与新内核，返回按类别计数的差异报告
///
/// # 前置条件
///
/// - 调用方已把**成对**的策略装好：`runner` 上每个 symbol 一个给出绝对目标的
///   `Strategy`，`kernel` 上每个 symbol 一个用 `qty * price / ctx.equity`
///   反推权益占比的 `StrategyHost`（两条路径必须共用同一个判据函数，否则比较的
///   是两份不同策略）；
/// - `kernel` 的 `KernelConfig` 必须与 `runner` 的配置同口径（`initial_cash` /
///   `force_liquidate` / `margin_specs` / 品种类型 / 资金费表 / 预算）；
/// - **不能在 tokio runtime 上下文里调用**：旧路径是 async，这里内部建专用
///   runtime 驱动，嵌套 `block_on` 会 panic。
pub fn run_shadow(
    scenario: &str,
    runner: &mut BacktestRunner,
    kernel: &mut Kernel,
    input: &ShadowInput,
) -> ShadowReport {
    let runtime = tokio::runtime::Runtime::new().expect("创建 shadow runtime 失败");
    let old = runtime.block_on(runner.run(
        &input.bars_by_symbol,
        &input.states_by_symbol,
        &input.funding_by_symbol,
        input.capital_budget,
    ));

    let mode = kernel.config().instrument_mode;
    let frames = match build_frames(&input.bars_by_symbol, &mode, Some(&input.states_by_symbol)) {
        Ok(frames) => frames,
        Err(e) => {
            return ShadowReport::failed(
                scenario,
                summarize_old(&old),
                format!("build_frames: {e}"),
            );
        }
    };
    let new = match kernel.run_history(frames) {
        Ok(result) => result,
        Err(e) => {
            return ShadowReport::failed(scenario, summarize_old(&old), format!("kernel: {e}"));
        }
    };

    let diffs = compare(&old, &new.run);
    let matched = diffs.iter().all(|diff| diff.count == 0);
    ShadowReport {
        scenario: scenario.to_string(),
        matched,
        diffs,
        old_summary: summarize_old(&old),
        new_summary: summarize_new(&new.run),
    }
}

/// 六类比较，顺序固定（报告稳定，便于跨次 diff 报告文件）
fn compare(old: &BacktestResult, new: &RunResult) -> Vec<ShadowDiff> {
    let mut nav = DiffCollector::new("nav_curve");
    compare_curve(
        &mut nav,
        "bar_nav_curve",
        &old.bar_nav_curve,
        &new.bar_nav_curve,
    );

    let mut balance = DiffCollector::new("balance_curve");
    compare_curve(
        &mut balance,
        "bar_balance_curve",
        &old.bar_balance_curve,
        &new.bar_balance_curve,
    );

    let mut fills = DiffCollector::new("fills");
    compare_fills(
        &mut fills,
        "fills_detail",
        &old.fills_detail,
        &new.fills_detail,
    );

    let mut trades = DiffCollector::new("trades");
    compare_trades(&mut trades, "trades", &old.trades, &new.trades);

    let mut positions = DiffCollector::new("positions");
    compare_positions(&mut positions, &old.final_positions, &new.positions);

    let mut totals = DiffCollector::new("totals");
    compare_totals(&mut totals, old, new);

    vec![
        nav.finish(),
        balance.finish(),
        fills.finish(),
        trades.finish(),
        positions.finish(),
        totals.finish(),
    ]
}

// ─────────────────────────────────────────────────────────────────────
// 差异收集器
// ─────────────────────────────────────────────────────────────────────

struct DiffCollector {
    category: &'static str,
    count: usize,
    samples: Vec<String>,
}

impl DiffCollector {
    fn new(category: &'static str) -> Self {
        Self {
            category,
            count: 0,
            samples: Vec::new(),
        }
    }

    /// 记一条差异；摘要用惰性闭包生成，避免为上千帧的曲线保留上千条字符串
    fn record(&mut self, detail: impl FnOnce() -> String) {
        self.count += 1;
        if self.samples.len() < MAX_SAMPLES {
            self.samples.push(detail());
        }
    }

    fn finish(self) -> ShadowDiff {
        ShadowDiff {
            category: self.category.to_string(),
            count: self.count,
            samples: self.samples,
        }
    }
}

/// f64 相对误差比较（0 附近退化为绝对误差）
fn f64_close(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    let diff = (a - b).abs();
    if diff <= ABS_TOL {
        return true;
    }
    diff <= REL_TOL * a.abs().max(b.abs())
}

// ─────────────────────────────────────────────────────────────────────
// 各类比较
// ─────────────────────────────────────────────────────────────────────

fn compare_curve(
    c: &mut DiffCollector,
    label: &str,
    old: &[(Timestamp, f64)],
    new: &[(Timestamp, f64)],
) {
    if old.len() != new.len() {
        c.record(|| format!("{label}: 帧数 {} vs {}", old.len(), new.len()));
    }
    for (i, (o, n)) in old.iter().zip(new.iter()).enumerate() {
        if o.0.nanos != n.0.nanos {
            c.record(|| format!("{label}[{i}]: 时间戳 {} vs {}", o.0.nanos, n.0.nanos));
        }
        if !f64_close(o.1, n.1) {
            c.record(|| format!("{label}[{i}]: 值 {} vs {} (ts={})", o.1, n.1, o.0.nanos));
        }
    }
}

fn compare_fills(c: &mut DiffCollector, label: &str, old: &[FillRecord], new: &[FillRecord]) {
    if old.len() != new.len() {
        c.record(|| format!("{label}: 条数 {} vs {}", old.len(), new.len()));
    }
    for (i, (o, n)) in old.iter().zip(new.iter()).enumerate() {
        if o.instrument != n.instrument {
            c.record(|| {
                format!(
                    "{label}[{i}].instrument: {} vs {}",
                    o.instrument, n.instrument
                )
            });
        }
        if o.taker_side != n.taker_side {
            c.record(|| {
                format!(
                    "{label}[{i}].side: {:?} vs {:?}",
                    o.taker_side, n.taker_side
                )
            });
        }
        if o.timestamp.nanos != n.timestamp.nanos {
            c.record(|| {
                format!(
                    "{label}[{i}].timestamp: {} vs {}",
                    o.timestamp.nanos, n.timestamp.nanos
                )
            });
        }
        if !f64_close(o.price.as_f64(), n.price.as_f64()) {
            c.record(|| {
                format!(
                    "{label}[{i}].price: {} vs {}",
                    o.price.as_f64(),
                    n.price.as_f64()
                )
            });
        }
        if !f64_close(o.quantity.as_f64(), n.quantity.as_f64()) {
            c.record(|| {
                format!(
                    "{label}[{i}].quantity: {} vs {}",
                    o.quantity.as_f64(),
                    n.quantity.as_f64()
                )
            });
        }
    }
}

fn compare_trades(c: &mut DiffCollector, label: &str, old: &[TradeRecord], new: &[TradeRecord]) {
    if old.len() != new.len() {
        c.record(|| format!("{label}: 条数 {} vs {}", old.len(), new.len()));
    }
    for (i, (o, n)) in old.iter().zip(new.iter()).enumerate() {
        let (ot, nt) = (&o.trade, &n.trade);
        if ot.timestamp.nanos != nt.timestamp.nanos {
            c.record(|| {
                format!(
                    "{label}[{i}].timestamp: {} vs {}",
                    ot.timestamp.nanos, nt.timestamp.nanos
                )
            });
        }
        if !f64_close(ot.price.as_f64(), nt.price.as_f64()) {
            c.record(|| {
                format!(
                    "{label}[{i}].price: {} vs {}",
                    ot.price.as_f64(),
                    nt.price.as_f64()
                )
            });
        }
        if !f64_close(ot.quantity.as_f64(), nt.quantity.as_f64()) {
            c.record(|| {
                format!(
                    "{label}[{i}].quantity: {} vs {}",
                    ot.quantity.as_f64(),
                    nt.quantity.as_f64()
                )
            });
        }
        if o.instrument != n.instrument {
            c.record(|| {
                format!(
                    "{label}[{i}].instrument: {} vs {}",
                    o.instrument, n.instrument
                )
            });
        }
        // 定点（×1e6）字段：反推往返可能差 1~2 微单位（见模块文档的归一化）
        for (name, a, b) in [
            ("realized_pnl", o.realized_pnl, n.realized_pnl),
            ("commission", o.commission, n.commission),
            ("net_quantity", o.net_quantity, n.net_quantity),
            ("entry_price", o.entry_price, n.entry_price),
        ] {
            if (a - b).abs() > MICRO_TOL {
                c.record(|| format!("{label}[{i}].{name}: {a} vs {b}"));
            }
        }
        if o.entry_time_ns != n.entry_time_ns {
            c.record(|| {
                format!(
                    "{label}[{i}].entry_time_ns: {} vs {}",
                    o.entry_time_ns, n.entry_time_ns
                )
            });
        }
    }
}

/// 终态非零持仓 map 比对（按 symbol 名归一化成有序 map，样本可读且稳定）
fn compare_positions(
    c: &mut DiffCollector,
    old: &[(Instrument, f64)],
    new: &HashMap<Instrument, f64>,
) {
    let old_map: BTreeMap<String, f64> = old
        .iter()
        .map(|(instrument, qty)| (instrument.to_string(), *qty))
        .collect();
    let new_map: BTreeMap<String, f64> = new
        .iter()
        .map(|(instrument, qty)| (instrument.to_string(), *qty))
        .collect();

    for (key, old_qty) in &old_map {
        match new_map.get(key) {
            None => c.record(|| format!("positions: 新路径缺少持仓 {key}")),
            Some(&new_qty) => {
                if !f64_close(*old_qty, new_qty) {
                    c.record(|| format!("positions[{key}]: {old_qty} vs {new_qty}"));
                }
            }
        }
    }
    for key in new_map.keys() {
        if !old_map.contains_key(key) {
            c.record(|| format!("positions: 旧路径缺少持仓 {key}"));
        }
    }
}

fn compare_totals(c: &mut DiffCollector, old: &BacktestResult, new: &RunResult) {
    for (name, a, b) in [
        ("final_nav", old.final_nav, new.final_nav),
        ("total_pnl", old.total_pnl, new.total_pnl),
        ("total_fees", old.total_fees, new.total_fees),
        (
            "total_funding_pnl",
            old.total_funding_pnl,
            new.total_funding_pnl,
        ),
        ("max_drawdown", old.max_drawdown, new.max_drawdown),
    ] {
        if !f64_close(a, b) {
            c.record(|| format!("totals.{name}: {a} vs {b}"));
        }
    }

    for (name, a, b) in [
        ("fills", old.fills, new.fills),
        ("orders_accepted", old.orders_accepted, new.orders_accepted),
        (
            "rebalances_triggered",
            old.rebalances_triggered,
            new.rebalances_triggered,
        ),
    ] {
        if a != b {
            c.record(|| format!("totals.{name}: {a} vs {b}"));
        }
    }

    if old.final_time_ns != new.final_time.nanos {
        c.record(|| {
            format!(
                "totals.final_time_ns: {} vs {}",
                old.final_time_ns, new.final_time.nanos
            )
        });
    }
}

// ─────────────────────────────────────────────────────────────────────
// 摘要
// ─────────────────────────────────────────────────────────────────────

/// 旧路径关键指标一行摘要（空格分隔的 `key=value`，测试可解析单个指标）
fn summarize_old(r: &BacktestResult) -> String {
    format!(
        "final_nav={:.6} pnl={:.6} fills={} trades={} orders_accepted={} fees={:.6} \
         funding={:.6} dd={:.6} final_time_ns={} rebalances={} liq={}",
        r.final_nav,
        r.total_pnl,
        r.fills,
        r.trades.len(),
        r.orders_accepted,
        r.total_fees,
        r.total_funding_pnl,
        r.max_drawdown,
        r.final_time_ns,
        r.rebalances_triggered,
        r.liquidation_count,
    )
}

/// 新内核关键指标一行摘要（口径与 [`summarize_old`] 逐项对齐）
fn summarize_new(r: &RunResult) -> String {
    format!(
        "final_nav={:.6} pnl={:.6} fills={} trades={} orders_accepted={} fees={:.6} \
         funding={:.6} dd={:.6} final_time_ns={} rebalances={} liq={}",
        r.final_nav,
        r.total_pnl,
        r.fills,
        r.trades.len(),
        r.orders_accepted,
        r.total_fees,
        r.total_funding_pnl,
        r.max_drawdown,
        r.final_time.nanos,
        r.rebalances_triggered,
        r.liquidation_count,
    )
}

#[cfg(test)]
mod tests {
    use axon_core::market::Side;
    use axon_core::types::{Price, Quantity, Symbol};

    use super::*;

    /// 比较器自身的单测：归一化口径（1e-9 相对 + 微单位容差）不能失效
    ///
    /// 若有人把 `f64_close` 改成逐位相等，或把 `MICRO_TOL` 抹成 0，这里先红。
    #[test]
    fn test_comparators_normalize_roundtrip_noise() {
        assert!(f64_close(0.3, 0.30000000000000004));
        assert!(!f64_close(1.0, 1.000_001));

        let mut c = DiffCollector::new("fills");
        let old = FillRecord::new(
            Timestamp::from_nanos(10),
            Instrument::from_symbol(&Symbol::from("BTC/USDT")),
            1,
            2,
            Side::Buy,
            Price::from_f64(100.0),
            Quantity::from_f64(0.3),
            0,
        );
        let mut new = old.clone();
        // 1 ulp 噪声：同一笔成交，价格/数量都在容差内
        new.price = Price::from_f64(100.0 + 1e-13);
        new.quantity = Quantity::from_f64(0.30000000000000004);
        compare_fills(&mut c, "fills_detail", std::slice::from_ref(&old), &[new]);
        assert_eq!(c.finish().count, 0, "1 ulp 噪声必须被归一化");

        // 真实差异必须被捕获
        let mut c = DiffCollector::new("fills");
        let mut worse = old.clone();
        worse.timestamp = Timestamp::from_nanos(11);
        compare_fills(&mut c, "fills_detail", std::slice::from_ref(&old), &[worse]);
        let diff = c.finish();
        assert_eq!(diff.count, 1, "时间戳不同必须计一条差异");
        assert!(diff.samples[0].contains("timestamp"));
    }

    #[test]
    fn test_report_samples_are_capped() {
        let mut c = DiffCollector::new("nav_curve");
        for i in 0..10 {
            c.record(|| format!("帧 {i}"));
        }
        let diff = c.finish();
        assert_eq!(diff.count, 10, "计数不受摘要上限影响");
        assert_eq!(diff.samples.len(), MAX_SAMPLES);
    }
}
