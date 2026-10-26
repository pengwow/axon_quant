//! 内核回测 ↔ `BacktestRunner::run()` 逐字段 parity（M1 验收门，spec Task 3.6）
//!
//! 同一份网格输入分别喂给两条路径，除墙钟 `duration` 外结果字段必须一致：
//!
//! - **A 路（语义真源）**：`axon_worker::BacktestRunner::run()`，策略自报**绝对数量**
//!   `target_position`。
//! - **B 路（新内核）**：`axon_kernel::Kernel::run_history()`，Host 吐
//!   `OrderIntent::SetTarget { target_ratio }`（**权益占比**），由 `BacktestSink`
//!   在唯一换算点转成绝对数量。
//!
//! # 为什么 Host 必须用 `ctx.equity` 反推 ratio
//!
//! `BacktestSink::submit` 的换算是硬编码的：
//!
//! ```text
//! qty = target_ratio * core.equity() / price
//! ```
//!
//! `BarLifecycle::on_bar_closed` 在决策前把 `HostCtx.equity` 回填为
//! `core.equity()`：同一帧、同一账务状态、同一个值（`SetTarget` 只登记目标，
//! 不动账务，所以 submit 时刻的 `core.equity()` 与决策时刻的 `ctx.equity`
//! 逐位相同）。于是 `target_ratio = q * price / ctx.equity` 能把绝对数量 `q`
//! 无损回推成 `q`。
//!
//! **不要**用 `initial_cash` 当基准：权益随回测漂移后
//! `qty = q * 当前权益 / initial_cash` 会系统性偏离 `q`，parity 立刻失败。
//!
//! # 为什么多品种场景要让「调仓错峰」
//!
//! 引擎 `bt_state.legs` 是 `HashMap`，`rebalance_to_target` 按它的迭代序发单，
//! 而 order_id 来自全局自增计数器。两个 `HashMap` 实例的迭代序不保证一致
//! （std 默认用随机哈希种子），同一 bar 两品种同时调仓会让「order_id → 品种」
//! 的分配在两条路径间漂移，而 `fills_detail` / `trades` 都带 order id。
//! 因此多品种用例设计成每个网格点最多只有一个品种改变目标（另一品种无 bar
//! 或走 Hold），把比较锚定在**编排语义**而不是哈希序上。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;

use axon_backtest::margin::LiquidationEvent;
use axon_backtest::{Bracket, MarginMode, MarginSpec};
use axon_core::Timestamp;
use axon_core::market::bar::Bar;
use axon_core::portfolio::FillRecord;
use axon_core::types::{Instrument, Price, Quantity, SwapSettle};
use axon_kernel::event::BarClosedEvent;
use axon_kernel::{
    BacktestSink, HostCtx, InstrumentMode, Kernel, KernelConfig, KernelResult, OrderIntent,
    StrategyHost, build_frames,
};
use axon_llm::decision::Action;
use axon_strategy::trait_::{BarContext, Strategy, StrategyDecision, StrategyError};
use axon_worker::{BacktestResult, BacktestRunner};

const START: i64 = 1_700_000_000_000_000_000;
const ONE_MIN: i64 = 60_000_000_000;

/// f64 相对误差（绝对值为 0 时退化为绝对误差 1e-12）
const REL_TOL: f64 = 1e-9;
const ABS_TOL: f64 = 1e-12;

// ─────────────────────────────────────────────────────────────────────
// 两条路径共用的策略判据
// ─────────────────────────────────────────────────────────────────────

/// 唯一判据：收盘 > 开盘 → 目标 +0.5；收盘 < 开盘 → 目标 -0.5；
/// 相等 → `None`（Hold，维持上次生效目标）。
///
/// A 路策略与 B 路 Host 必须调用同一个函数，否则比较的是两份不同策略，
/// parity 就失去意义。
fn target_quantity(bar: &Bar) -> Option<f64> {
    let open = bar.open.as_f64();
    let close = bar.close.as_f64();
    if close > open {
        Some(0.5)
    } else if close < open {
        Some(-0.5)
    } else {
        None
    }
}

/// A 路：直接给出绝对目标数量的 `Strategy`
struct AbsoluteStrategy {
    name: String,
    symbol: String,
}

#[async_trait]
impl Strategy for AbsoluteStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn symbol(&self) -> &str {
        &self.symbol
    }

    async fn decide(&self, ctx: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
        match target_quantity(ctx.bar) {
            Some(q) => {
                // A 路语义：`target_position` 是绝对数量，Action 只决定方向
                let action = if q > 0.0 { Action::Buy } else { Action::Sell };
                Ok(StrategyDecision::from_rule(action).with_target_position(q))
            }
            None => Ok(StrategyDecision::from_rule(Action::Hold)),
        }
    }
}

/// B 路：把同一判据给出的绝对数量换算成权益占比的 `StrategyHost`
struct RatioHost {
    instrument: Instrument,
}

impl StrategyHost for RatioHost {
    fn name(&self) -> &str {
        "ratio_host"
    }

    fn on_event(&mut self, ev: &BarClosedEvent<'_>, ctx: &HostCtx) -> Vec<OrderIntent> {
        // 固定 symbol 的 Host 只处理自己的品种（与 A 路每个策略只认自己 symbol 对齐）
        if ev.instrument != &self.instrument {
            return Vec::new();
        }
        let Some(q) = target_quantity(ev.bar) else {
            // 空 vec = Hold（维持上次生效目标），不是平仓
            return Vec::new();
        };
        let price = ctx.last_price.get(&self.instrument).copied().unwrap_or(0.0);
        if price <= 0.0 || ctx.equity <= 0.0 {
            // 无价 / 无权益时无法换算；本测试的数据不会走到这里
            return Vec::new();
        }
        vec![OrderIntent::SetTarget {
            instrument: self.instrument.clone(),
            // 见模块文档：用 ctx.equity 才能把 q 无损回推（BacktestSink 的换算基准）
            target_ratio: q * price / ctx.equity,
        }]
    }
}

// ─────────────────────────────────────────────────────────────────────
// 用例装配：同一份输入跑两条路径
// ─────────────────────────────────────────────────────────────────────

/// 一次 parity 用例的运行参数（除 bars 外的全部外部输入）
struct CaseConfig {
    initial_cash: f64,
    force_liquidate: bool,
    funding: BTreeMap<String, BTreeMap<i64, (f64, f64)>>,
    margin_specs: HashMap<String, MarginSpec>,
    /// `Some((settle, contract_size))` = 永续模式，`None` = 现货
    futures: Option<(SwapSettle, f64)>,
}

impl Default for CaseConfig {
    fn default() -> Self {
        Self {
            initial_cash: 100_000.0,
            force_liquidate: false,
            funding: BTreeMap::new(),
            margin_specs: HashMap::new(),
            futures: None,
        }
    }
}

/// 同一份网格输入分别跑 A / B 两路并逐字段比对
async fn run_case(
    label: &str,
    bars_by_symbol: &BTreeMap<String, Vec<Option<Bar>>>,
    cfg: &CaseConfig,
) -> (BacktestResult, KernelResult) {
    let grid_len = bars_by_symbol.values().next().map(Vec::len).unwrap_or(0);

    // ── A 路：BacktestRunner（语义真源）──
    let mut runner = BacktestRunner::simple(cfg.initial_cash)
        .with_force_liquidate(cfg.force_liquidate)
        .with_margin(cfg.margin_specs.clone());
    if let Some((settle, contract_size)) = cfg.futures {
        runner = runner.with_futures(settle, contract_size);
    }
    for symbol in bars_by_symbol.keys() {
        runner.add_strategy(Arc::new(AbsoluteStrategy {
            name: format!("abs_{symbol}"),
            symbol: symbol.clone(),
        }));
    }
    let states: BTreeMap<String, Vec<serde_json::Value>> = bars_by_symbol
        .keys()
        .map(|symbol| (symbol.clone(), vec![serde_json::Value::Null; grid_len]))
        .collect();
    let a = runner
        .run(bars_by_symbol, &states, &cfg.funding, None)
        .await;

    // ── B 路：Kernel + BacktestSink ──
    let mode = match cfg.futures {
        Some((settle, contract_size)) => InstrumentMode::Swap {
            settle,
            contract_size,
        },
        None => InstrumentMode::Spot,
    };
    let kernel_config = KernelConfig {
        initial_cash: cfg.initial_cash,
        force_liquidate: cfg.force_liquidate,
        margin_specs: cfg.margin_specs.clone(),
        instrument_mode: mode,
        funding_by_symbol: cfg.funding.clone(),
        ..KernelConfig::default()
    };
    let mut kernel = Kernel::new(kernel_config, Box::new(BacktestSink::new()));
    let mut hosts = 0usize;
    for symbol in bars_by_symbol.keys() {
        kernel.add_host(Box::new(RatioHost {
            instrument: mode.instrument(symbol),
        }));
        hosts += 1;
    }
    let frames = build_frames(bars_by_symbol, &mode, None).expect("网格应能展平成帧");
    let b = kernel.run_history(frames).expect("内核回测应跑通");

    assert_parity(label, &a, &b, hosts);
    (a, b)
}

// ─────────────────────────────────────────────────────────────────────
// 逐字段比较
// ─────────────────────────────────────────────────────────────────────

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

fn check_f64(label: &str, a: f64, b: f64) {
    assert!(
        f64_close(a, b),
        "{label} 不一致：A={a} B={b}，相对差 {}",
        (a - b).abs() / a.abs().max(b.abs()).max(f64::MIN_POSITIVE)
    );
}

fn check_curve(label: &str, a: &[(Timestamp, f64)], b: &[(Timestamp, f64)]) {
    assert_eq!(
        a.len(),
        b.len(),
        "{label} 长度不一致：{} vs {}",
        a.len(),
        b.len()
    );
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(x.0.nanos, y.0.nanos, "{label}[{i}] 时间戳不一致");
        check_f64(&format!("{label}[{i}].value"), x.1, y.1);
    }
}

fn check_positions(label: &str, a: &[(Instrument, f64)], b: &HashMap<Instrument, f64>) {
    assert_eq!(
        a.len(),
        b.len(),
        "{label} 持仓数量不一致：{} vs {}",
        a.len(),
        b.len()
    );
    for (instrument, qty) in a {
        let other = b
            .get(instrument)
            .copied()
            .unwrap_or_else(|| panic!("{label} 内核缺少持仓 {instrument}"));
        check_f64(&format!("{label}[{instrument}]"), *qty, other);
    }
}

fn check_fills(label: &str, a: &[FillRecord], b: &[FillRecord]) {
    assert_eq!(
        a.len(),
        b.len(),
        "{label} 长度不一致：{} vs {}",
        a.len(),
        b.len()
    );
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(x.fill_id, y.fill_id, "{label}[{i}].fill_id 不一致");
        assert_eq!(
            x.taker_order_id, y.taker_order_id,
            "{label}[{i}].taker_order_id 不一致"
        );
        assert_eq!(
            x.maker_order_id, y.maker_order_id,
            "{label}[{i}].maker_order_id 不一致"
        );
        assert_eq!(x.instrument, y.instrument, "{label}[{i}].instrument 不一致");
        assert_eq!(x.taker_side, y.taker_side, "{label}[{i}].taker_side 不一致");
        assert_eq!(
            x.timestamp.nanos, y.timestamp.nanos,
            "{label}[{i}].timestamp 不一致"
        );
        check_f64(
            &format!("{label}[{i}].price"),
            x.price.as_f64(),
            y.price.as_f64(),
        );
        check_f64(
            &format!("{label}[{i}].quantity"),
            x.quantity.as_f64(),
            y.quantity.as_f64(),
        );
    }
}

fn check_liquidations(label: &str, a: &[LiquidationEvent], b: &[LiquidationEvent]) {
    assert_eq!(
        a.len(),
        b.len(),
        "{label} 长度不一致：{} vs {}",
        a.len(),
        b.len()
    );
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(x.timestamp_ns, y.timestamp_ns, "{label}[{i}].timestamp_ns");
        assert_eq!(x.symbol, y.symbol, "{label}[{i}].symbol");
        assert_eq!(x.margin_mode, y.margin_mode, "{label}[{i}].margin_mode");
        assert_eq!(x.tier_from, y.tier_from, "{label}[{i}].tier_from");
        assert_eq!(x.tier_to, y.tier_to, "{label}[{i}].tier_to");
        check_f64(
            &format!("{label}[{i}].reduced_qty"),
            x.reduced_qty,
            y.reduced_qty,
        );
        check_f64(&format!("{label}[{i}].price"), x.price, y.price);
        check_f64(&format!("{label}[{i}].fee"), x.fee, y.fee);
        assert_eq!(x.reason, y.reason, "{label}[{i}].reason");
        assert_eq!(x.price_basis, y.price_basis, "{label}[{i}].price_basis");
    }
}

/// 逐字段 parity 断言
///
/// 排除项只有 `duration`（墙钟耗时，两条路径必然不同）；
/// `decisions_timeline` 是 A 路独有的策略轨迹，内核侧对应物是 journal，
/// 不属于结果契约，故不比较。
fn assert_parity(label: &str, a: &BacktestResult, b: &KernelResult, hosts: usize) {
    let br = &b.run;

    // ── 计数类：逐位相等 ──
    assert_eq!(
        a.events_processed, br.events_processed,
        "{label}: events_processed"
    );
    assert_eq!(
        a.orders_accepted, br.orders_accepted,
        "{label}: orders_accepted"
    );
    assert_eq!(
        a.orders_rejected, br.orders_rejected,
        "{label}: orders_rejected"
    );
    assert_eq!(
        a.orders_cancelled, br.orders_cancelled,
        "{label}: orders_cancelled"
    );
    assert_eq!(
        a.orders_modified, br.orders_modified,
        "{label}: orders_modified"
    );
    assert_eq!(a.fills, br.fills, "{label}: fills");
    assert_eq!(
        a.rebalances_triggered, br.rebalances_triggered,
        "{label}: rebalances_triggered"
    );
    assert_eq!(
        a.final_time_ns, br.final_time.nanos,
        "{label}: final_time_ns"
    );
    assert_eq!(
        a.n_bars, b.bars_processed,
        "{label}: n_bars / bars_processed"
    );
    assert_eq!(
        a.n_strategies as usize, hosts,
        "{label}: n_strategies / hosts"
    );

    // ── 账务 / 指标：f64 相对误差 ──
    check_f64(&format!("{label}: total_pnl"), a.total_pnl, br.total_pnl);
    check_f64(
        &format!("{label}: max_drawdown"),
        a.max_drawdown,
        br.max_drawdown,
    );
    check_f64(
        &format!("{label}: max_drawdown_pct"),
        a.max_drawdown_pct,
        br.max_drawdown_pct,
    );
    check_f64(&format!("{label}: nav_peak"), a.nav_peak, br.nav_peak);
    check_f64(&format!("{label}: final_nav"), a.final_nav, br.final_nav);
    check_f64(&format!("{label}: total_fees"), a.total_fees, br.total_fees);
    check_f64(
        &format!("{label}: total_funding_pnl"),
        a.total_funding_pnl,
        br.total_funding_pnl,
    );
    check_f64(&format!("{label}: win_rate"), a.win_rate, br.win_rate);
    check_f64(
        &format!("{label}: sharpe_ratio"),
        a.sharpe_ratio,
        br.sharpe_ratio,
    );
    check_f64(
        &format!("{label}: insurance_fund_loss"),
        a.insurance_fund_loss,
        br.insurance_fund_loss,
    );

    // ── 曲线：先比长度，再逐帧比 (timestamp, value) ──
    check_curve(
        &format!("{label}: bar_nav_curve"),
        &a.bar_nav_curve,
        &br.bar_nav_curve,
    );
    check_curve(
        &format!("{label}: bar_balance_curve"),
        &a.bar_balance_curve,
        &br.bar_balance_curve,
    );
    check_curve(
        &format!("{label}: equity_curve"),
        &a.equity_curve,
        &br.equity_curve,
    );

    // ── 明细 ──
    check_positions(
        &format!("{label}: final_positions"),
        &a.final_positions,
        &br.positions,
    );
    check_fills(
        &format!("{label}: fills_detail"),
        &a.fills_detail,
        &br.fills_detail,
    );
    assert_eq!(
        a.trades.len(),
        br.trades.len(),
        "{label}: trades 数量不一致"
    );
    for (i, (x, y)) in a.trades.iter().zip(br.trades.iter()).enumerate() {
        assert_eq!(x, y, "{label}: trades[{i}] 不一致");
    }
    check_liquidations(
        &format!("{label}: liquidation_events"),
        &a.liquidation_events,
        &br.liquidation_events,
    );

    // ── 保证金口径 ──
    assert_eq!(
        a.margin_applied, br.margin_applied,
        "{label}: margin_applied"
    );
    assert_eq!(
        a.margin_specs_effective, br.margin_specs_effective,
        "{label}: margin_specs_effective"
    );
    assert_eq!(
        a.liquidation_count, br.liquidation_count,
        "{label}: liquidation_count"
    );
    assert_eq!(
        a.margin_shortfall_count, br.margin_shortfall_count,
        "{label}: margin_shortfall_count"
    );
    assert_eq!(a.price_basis, br.price_basis, "{label}: price_basis");
}

// ─────────────────────────────────────────────────────────────────────
// 数据构造
// ─────────────────────────────────────────────────────────────────────

fn bar_at(ts: i64, open: f64, close: f64) -> Bar {
    Bar {
        timestamp: Timestamp::from_nanos(ts),
        open: Price::from_f64(open),
        high: Price::from_f64(open.max(close) + 0.4),
        low: Price::from_f64(open.min(close) - 0.4),
        close: Price::from_f64(close),
        volume: Quantity::from_f64(1_000.0),
    }
}

/// 有涨有跌的之字形序列：`(open 偏移, close 偏移)` 四态循环，叠加缓慢漂移，
/// 覆盖 Buy / Sell / Hold（相等态）三种决策。
fn zigzag_bars(n: usize, start_price: f64) -> Vec<Option<Bar>> {
    const PATTERN: [(f64, f64); 4] = [(0.0, 1.0), (1.0, 0.0), (0.0, 0.0), (0.0, -1.0)];
    (0..n)
        .map(|i| {
            let (open_delta, close_delta) = PATTERN[i % PATTERN.len()];
            let base = start_price + i as f64 * 0.25;
            Some(bar_at(
                START + i as i64 * ONE_MIN,
                base + open_delta,
                base + close_delta,
            ))
        })
        .collect()
}

fn btc_margin_spec() -> MarginSpec {
    MarginSpec {
        leverage: 10.0,
        mode: MarginMode::Cross,
        brackets: vec![Bracket {
            notional_floor: 0.0,
            notional_cap: None,
            mmr: 0.005,
            cum: 0.0,
            liquidation_fee_rate: 0.001,
            max_leverage: 125.0,
        }],
        bracket_source: "default".into(),
    }
}

// ─────────────────────────────────────────────────────────────────────
// 用例
// ─────────────────────────────────────────────────────────────────────

/// 场景 1：单品种、30 根 1m bar、价格有涨有跌（多次 Buy / Sell / Hold）
#[tokio::test]
async fn case1_single_symbol_30_bars_zigzag() {
    let bars = BTreeMap::from([("BTC/USDT".to_string(), zigzag_bars(30, 100.0))]);
    let (a, b) = run_case("case1", &bars, &CaseConfig::default()).await;

    // 前置条件：用例必须真的产生成交与 round-trip，否则 parity 断言没有力度
    assert!(a.fills > 0, "case1 应产生成交，实际 fills={}", a.fills);
    assert!(
        a.trades.len() >= 4,
        "case1 应产生多次反手/平仓的 round-trip，实际 {}",
        a.trades.len()
    );
    assert!(!a.final_positions.is_empty(), "case1 末态应仍有持仓");
    assert_eq!(a.bar_nav_curve.len(), 30, "每帧一帧 NAV");
    assert!(a.total_fees > 0.0, "有成交就必然有手续费");
    assert!(a.max_drawdown > 0.0, "涨跌交替应产生非零回撤");
    assert_eq!(b.bars_processed, 30, "内核应处理 30 帧");
}

/// 场景 2：两品种、等长网格、带 `None` 缺口
///
/// BTC 12 根；ETH 只在奇数网格点有 bar（偶数点是缺口）。奇数点 BTC 走 Hold，
/// 偶数点 ETH 无 bar —— 每个网格点最多一个品种调仓（原因见模块文档的哈希序说明）。
#[tokio::test]
async fn case2_two_symbols_grid_with_none_gaps() {
    const N: usize = 12;

    let btc: Vec<Option<Bar>> = (0..N)
        .map(|i| {
            let ts = START + i as i64 * ONE_MIN;
            let base = 100.0 + i as f64 * 0.1;
            if i % 2 == 0 {
                // 偶数点：有方向 → 调仓
                let up = (i / 2) % 2 == 0;
                Some(if up {
                    bar_at(ts, base, base + 1.0)
                } else {
                    bar_at(ts, base, base - 1.0)
                })
            } else {
                // 奇数点：开收相等 → Hold
                Some(bar_at(ts, base, base))
            }
        })
        .collect();

    let eth: Vec<Option<Bar>> = (0..N)
        .map(|i| {
            if i % 2 == 0 {
                return None; // 缺口：价格由内核前向填充
            }
            let ts = START + i as i64 * ONE_MIN;
            let base = 20.0 + i as f64 * 0.02;
            let up = ((i - 1) / 2) % 2 == 0;
            Some(if up {
                bar_at(ts, base, base + 0.6)
            } else {
                bar_at(ts, base, base - 0.6)
            })
        })
        .collect();

    let bars = BTreeMap::from([("BTC/USDT".to_string(), btc), ("ETH/USDT".to_string(), eth)]);
    let (a, b) = run_case("case2", &bars, &CaseConfig::default()).await;

    assert_eq!(b.bars_processed, N as u64, "缺口帧也必须触发生命周期");
    // 两品种都必须真的产生成交（否则跨品种口径没被验证到）
    let mut filled: Vec<String> = a
        .fills_detail
        .iter()
        .map(|f| format!("{}", f.instrument))
        .collect();
    filled.sort();
    filled.dedup();
    assert_eq!(
        filled,
        vec!["BTC/USDT".to_string(), "ETH/USDT".to_string()],
        "两个品种都应有成交"
    );
    assert_eq!(a.final_positions.len(), 2, "末态应同时持有两个品种");
}

/// 场景 3：`force_liquidate = true` 且末尾有持仓 → 验证 EOD 强平（`finalize` 语义）
#[tokio::test]
async fn case3_force_liquidate_closes_open_positions() {
    let bars = BTreeMap::from([("BTC/USDT".to_string(), zigzag_bars(8, 100.0))]);
    let cfg = CaseConfig {
        force_liquidate: true,
        ..CaseConfig::default()
    };
    let (a, b) = run_case("case3", &bars, &cfg).await;

    assert!(
        a.final_positions.is_empty(),
        "EOD 强平后 A 路不应残留持仓：{:?}",
        a.final_positions
    );
    assert!(
        b.run.positions.is_empty(),
        "EOD 强平后 B 路不应残留持仓：{:?}",
        b.run.positions
    );
    assert!(a.fills > 0, "应有开仓 + EOD 平仓成交");
}

/// 场景 4：`funding_by_symbol` 两个结算点（rate 非零），一个 mark 缺省走 close 兜底
///
/// 资金费只对永续合约生效（引擎对 `Instrument::Spot` 的 Funding 事件直接忽略），
/// 因此本用例切到 `Swap` + USD 保证金结算。
#[tokio::test]
async fn case4_funding_settlements() {
    let bars = BTreeMap::from([("BTC/USDT".to_string(), zigzag_bars(10, 100.0))]);

    let mut schedule: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
    // 第 3 帧：mark = 0 → 两侧都必须用该品种最近 close 兜底
    schedule.insert(START + 3 * ONE_MIN, (0.0001, 0.0));
    // 第 7 帧：显式 mark，rate 为负（付款方向反向）
    schedule.insert(START + 7 * ONE_MIN, (-0.0002, 101.0));
    let cfg = CaseConfig {
        funding: BTreeMap::from([("BTC/USDT".to_string(), schedule)]),
        futures: Some((SwapSettle::UsdMargin, 1.0)),
        ..CaseConfig::default()
    };

    let (a, _b) = run_case("case4", &bars, &cfg).await;

    assert_eq!(a.events_processed, 2, "两个结算点 = 两个 Funding 事件");
    assert!(
        a.total_funding_pnl.abs() > 0.0,
        "非零 rate 必须产生资金费 PnL，实际 {}",
        a.total_funding_pnl
    );
}

/// 场景 5：启用保证金（`margin_specs` 非空）→ 预算停用 + 逐仓/全仓账务路径
#[tokio::test]
async fn case5_margin_specs_enabled() {
    let bars = BTreeMap::from([("BTC/USDT".to_string(), zigzag_bars(12, 100.0))]);
    let cfg = CaseConfig {
        initial_cash: 10_000.0,
        margin_specs: HashMap::from([("BTC/USDT".to_string(), btc_margin_spec())]),
        ..CaseConfig::default()
    };
    let (a, b) = run_case("case5", &bars, &cfg).await;

    assert!(a.margin_applied, "A 路应启用保证金账务");
    assert!(b.run.margin_applied, "B 路应启用保证金账务");
    assert_eq!(a.margin_specs_effective.len(), 1, "规格回显应有一个品种");
    assert!(a.fills > 0, "保证金场景也必须真的成交");
}
