//! `BarContextSim` ↔ `BacktestRunner` 逐字段 parity（M2 Task 5 验收）
//!
//! 同一份网格输入 + 逐 bar state 分别喂给两条路径：
//!
//! - **A 路**：`BacktestRunner::run()`，策略直接吃 `BarContext`（含 `state`），
//!   自报**绝对数量** `target_position`。
//! - **B 路**：`Kernel::run_history()` + `BarContextSim`，桥接把同一个
//!   `Strategy::decide()` 的绝对数量用 `qty * price / ctx.equity` 反推成
//!   `OrderIntent::SetTarget`（权益占比），再由 `BacktestSink` 换回数量。
//!
//! 若 `BarContextSim` 的逆运算与 `BacktestSink` 的换算不互逆（比如误用
//! `initial_cash` 当基准），目标数量会随权益漂移产生系统性偏差，
//! `total_pnl` / 持仓 / 成交明细立刻不一致。
//!
//! 说明：本测试用普通 `#[test]` 而不是 `#[tokio::test]`——`BarContextSim`
//! 内部对策略 `decide()` 调 `Handle::block_on`，必须在 runtime 上下文之外调用
//! （嵌套 `block_on` 会 panic）。A 路先 `Runtime::block_on` 跑完退出，B 路再
//! 用同一个 runtime 的 `Handle` 驱动策略，两者顺序执行、互不嵌套。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;

use axon_backtest::margin::LiquidationEvent;
use axon_backtest::{Bracket, MarginMode, MarginSpec};
use axon_core::Timestamp;
use axon_core::market::bar::Bar;
use axon_core::portfolio::{FillRecord, TradeRecord};
use axon_core::types::{Instrument, Price, Quantity, SwapSettle};
use axon_kernel::{BacktestSink, InstrumentMode, Kernel, KernelConfig, KernelResult, build_frames};
use axon_llm::decision::Action;
use axon_strategy::trait_::{BarContext, Strategy, StrategyDecision, StrategyError};
use axon_worker::kernel_host::BarContextSim;
use axon_worker::{BacktestResult, BacktestRunner};

const START: i64 = 1_700_000_000_000_000_000;
const ONE_MIN: i64 = 60_000_000_000;

/// f64 相对误差（绝对值为 0 时退化为绝对误差 1e-12）
const REL_TOL: f64 = 1e-9;
const ABS_TOL: f64 = 1e-12;

// ─────────────────────────────────────────────────────────────────────
// 两条路径共用的策略（读 state，给出绝对目标数量）
// ─────────────────────────────────────────────────────────────────────

/// `state.features.sma5_vs_sma20`：正 → Buy +0.5 单位；负 → Sell -0.3；0 → Hold
///
/// `Hold` 不设 `target_position`：内核侧 `BarContextSim` 返回空 vec（沿用上次
/// 生效目标），A 路 `BacktestRunner` 同样沿用上次目标 —— 语义必须一致。
struct StateAbsoluteStrategy {
    name: String,
    symbol: String,
}

#[async_trait]
impl Strategy for StateAbsoluteStrategy {
    fn name(&self) -> &str {
        &self.name
    }

    fn symbol(&self) -> &str {
        &self.symbol
    }

    async fn decide(&self, ctx: &BarContext<'_>) -> Result<StrategyDecision, StrategyError> {
        let sma = ctx
            .state
            .get("features")
            .and_then(|f| f.get("sma5_vs_sma20"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if sma > 0.0 {
            Ok(StrategyDecision::from_rule(Action::Buy).with_target_position(0.5))
        } else if sma < 0.0 {
            Ok(StrategyDecision::from_rule(Action::Sell).with_target_position(-0.3))
        } else {
            Ok(StrategyDecision::from_rule(Action::Hold))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// 用例装配
// ─────────────────────────────────────────────────────────────────────

/// 一次 parity 用例的运行参数
struct CaseConfig {
    initial_cash: f64,
    force_liquidate: bool,
    margin_specs: HashMap<String, MarginSpec>,
    /// `Some((settle, contract_size))` = 永续模式，`None` = 现货
    futures: Option<(SwapSettle, f64)>,
}

impl Default for CaseConfig {
    fn default() -> Self {
        Self {
            initial_cash: 100_000.0,
            force_liquidate: false,
            margin_specs: HashMap::new(),
            futures: None,
        }
    }
}

/// 同一份输入分别跑 A / B 两路并逐字段比对
fn run_case(
    label: &str,
    bars_by_symbol: &BTreeMap<String, Vec<Option<Bar>>>,
    states_by_symbol: &BTreeMap<String, Vec<serde_json::Value>>,
    cfg: &CaseConfig,
) -> (BacktestResult, KernelResult) {
    let runtime = tokio::runtime::Runtime::new().expect("创建 tokio runtime");

    // ── A 路：BacktestRunner（语义真源）──
    let mut runner = BacktestRunner::simple(cfg.initial_cash)
        .with_force_liquidate(cfg.force_liquidate)
        .with_margin(cfg.margin_specs.clone());
    if let Some((settle, contract_size)) = cfg.futures {
        runner = runner.with_futures(settle, contract_size);
    }
    for symbol in bars_by_symbol.keys() {
        runner.add_strategy(Arc::new(StateAbsoluteStrategy {
            name: format!("state_abs_{symbol}"),
            symbol: symbol.clone(),
        }));
    }
    let a = runtime.block_on(runner.run(bars_by_symbol, states_by_symbol, &BTreeMap::new(), None));

    // ── B 路：Kernel + BarContextSim ──
    let mode = match cfg.futures {
        Some((settle, contract_size)) => InstrumentMode::Swap {
            settle,
            contract_size,
        },
        None => InstrumentMode::Spot,
    };
    let frames =
        build_frames(bars_by_symbol, &mode, Some(states_by_symbol)).expect("网格应能展平成帧");
    let config = KernelConfig {
        initial_cash: cfg.initial_cash,
        force_liquidate: cfg.force_liquidate,
        margin_specs: cfg.margin_specs.clone(),
        instrument_mode: mode,
        ..KernelConfig::default()
    };
    let mut kernel = Kernel::new(config, Box::new(BacktestSink::new()));
    for symbol in bars_by_symbol.keys() {
        kernel.add_host(Box::new(BarContextSim::new(
            Arc::new(StateAbsoluteStrategy {
                name: format!("state_abs_{symbol}"),
                symbol: symbol.clone(),
            }),
            mode.instrument(symbol),
            runtime.handle().clone(),
        )));
    }
    let b = kernel.run_history(frames).expect("内核回测应跑通");

    assert_parity(label, &a, &b, bars_by_symbol.len(), cfg.initial_cash);
    (a, b)
}

// ─────────────────────────────────────────────────────────────────────
// 逐字段比较（与 `kernel_parity.rs` 同口径）
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

/// i64 微单位（× 1e6）账务字段的容差
///
/// `qty * price / equity` 的反推往返在 f64 最末位上可能有 1 ulp 误差
/// （如 0.3 → 0.29999999999999993），量化到 ×1e6 后最多差 1~2 微单位。
/// 这不是语义偏差；id / 时间戳 / fill 关联仍逐位比较。
const MICRO_TOL: i64 = 2;

fn check_micro(label: &str, a: i64, b: i64) {
    assert!(
        (a - b).abs() <= MICRO_TOL,
        "{label} 不一致：A={a} B={b}（差 {} 微单位）",
        (a - b).abs()
    );
}

fn check_trades(label: &str, a: &[TradeRecord], b: &[TradeRecord]) {
    assert_eq!(
        a.len(),
        b.len(),
        "{label} 长度不一致：{} vs {}",
        a.len(),
        b.len()
    );
    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        // 数量/价格用相对容差：`qty * price / equity` 的反推往返本身可能差 1 ulp
        // （0.5 → 0.5000000000000001），这不是语义偏差；id / 时间戳 / fill 关联
        // 逐位相等，微单位账务字段用 `check_micro` 的 1~2 微单位容差。
        assert_eq!(
            x.trade.timestamp.nanos, y.trade.timestamp.nanos,
            "{label}[{i}].timestamp"
        );
        assert_eq!(
            x.trade.buyer_order_id, y.trade.buyer_order_id,
            "{label}[{i}].buyer_order_id"
        );
        assert_eq!(
            x.trade.seller_order_id, y.trade.seller_order_id,
            "{label}[{i}].seller_order_id"
        );
        check_f64(
            &format!("{label}[{i}].price"),
            x.trade.price.as_f64(),
            y.trade.price.as_f64(),
        );
        check_f64(
            &format!("{label}[{i}].quantity"),
            x.trade.quantity.as_f64(),
            y.trade.quantity.as_f64(),
        );
        check_micro(
            &format!("{label}[{i}].realized_pnl"),
            x.realized_pnl,
            y.realized_pnl,
        );
        check_micro(
            &format!("{label}[{i}].commission"),
            x.commission,
            y.commission,
        );
        check_micro(
            &format!("{label}[{i}].net_quantity"),
            x.net_quantity,
            y.net_quantity,
        );
        assert_eq!(x.instrument, y.instrument, "{label}[{i}].instrument");
        check_micro(
            &format!("{label}[{i}].entry_price"),
            x.entry_price,
            y.entry_price,
        );
        assert_eq!(
            x.entry_time_ns, y.entry_time_ns,
            "{label}[{i}].entry_time_ns"
        );
        assert_eq!(
            x.entry_fill_ids, y.entry_fill_ids,
            "{label}[{i}].entry_fill_ids"
        );
        assert_eq!(
            x.exit_fill_ids, y.exit_fill_ids,
            "{label}[{i}].exit_fill_ids"
        );
    }
}

/// 逐字段 parity 断言（排除墙钟 `duration` 与 A 路独有的 `decisions_timeline`）
fn assert_parity(
    label: &str,
    a: &BacktestResult,
    b: &KernelResult,
    hosts: usize,
    initial_cash: f64,
) {
    let br = &b.run;

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
    assert_eq!(a.n_bars, b.bars_processed, "{label}: n_bars");
    assert_eq!(a.n_strategies as usize, hosts, "{label}: n_strategies");

    check_f64(&format!("{label}: total_pnl"), a.total_pnl, br.total_pnl);
    // 同一份输入下 total_pnl = final_nav - initial_cash，两条路径都必须成立；
    // 任一侧用了别的基准（如误用 initial_cash 反推 ratio）这里会先炸
    check_f64(
        &format!("{label}: total_pnl 自洽"),
        a.total_pnl,
        a.final_nav - initial_cash,
    );
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
    check_trades(&format!("{label}: trades"), &a.trades, &br.trades);
    check_liquidations(
        &format!("{label}: liquidation_events"),
        &a.liquidation_events,
        &br.liquidation_events,
    );

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

/// 有涨有跌的之字形序列
fn zigzag_bars(n: usize, start_price: f64) -> Vec<Option<Bar>> {
    const PATTERN: [(f64, f64); 3] = [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0)];
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

fn state(value: f64) -> serde_json::Value {
    serde_json::json!({"features": {"sma5_vs_sma20": value}})
}

/// 单品种逐 bar state：买 / 卖 / 观望 三态循环，且与 bar 涨跌独立
fn state_column(n: usize) -> Vec<serde_json::Value> {
    (0..n)
        .map(|i| match i % 3 {
            0 => state(0.02),
            1 => state(-0.02),
            _ => state(0.0),
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

/// 场景 1：单品种 30 根 bar，逐 bar state 驱动 Buy / Sell / Hold
#[test]
fn case1_single_symbol_states_drive_decisions() {
    const N: usize = 30;
    let bars = BTreeMap::from([("BTC/USDT".to_string(), zigzag_bars(N, 100.0))]);
    let states = BTreeMap::from([("BTC/USDT".to_string(), state_column(N))]);

    let (a, b) = run_case("case1", &bars, &states, &CaseConfig::default());

    // 前置条件：必须真的成交、反手，否则 parity 没有力度
    assert!(a.fills > 0, "case1 应产生成交，实际 fills={}", a.fills);
    assert!(
        a.trades.len() >= 2,
        "应产生多次反手/平仓，实际 {}",
        a.trades.len()
    );
    assert!(!a.final_positions.is_empty(), "末态应仍有持仓");
    assert!(a.total_fees > 0.0, "有成交就必然有手续费");
    assert_eq!(b.bars_processed, N as u64, "内核应处理全部帧");
}

/// 场景 2：两品种 + `None` 缺口，逐 bar state 只在有真实 bar 的网格点决策
///
/// 每个网格点最多一个品种改变目标（BTC 在奇数点 Hold、ETH 在偶数点无 bar），
/// 避开引擎 `HashMap` 迭代序带来的 order_id 漂移（见 `kernel_parity.rs` 说明）。
#[test]
fn case2_two_symbols_with_state_gaps() {
    const N: usize = 12;

    let btc: Vec<Option<Bar>> = (0..N)
        .map(|i| {
            let ts = START + i as i64 * ONE_MIN;
            let base = 100.0 + i as f64 * 0.1;
            if i % 2 == 0 {
                let up = (i / 2) % 2 == 0;
                Some(if up {
                    bar_at(ts, base, base + 1.0)
                } else {
                    bar_at(ts, base, base - 1.0)
                })
            } else {
                Some(bar_at(ts, base, base))
            }
        })
        .collect();
    let eth: Vec<Option<Bar>> = (0..N)
        .map(|i| {
            if i % 2 == 0 {
                return None;
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

    // BTC 奇数点 state = 0（Hold），偶数点买卖交替；ETH 只在奇数点有 bar，
    // 偶数点 state 随意（不会被决策）。
    let mut states = BTreeMap::new();
    states.insert(
        "BTC/USDT".to_string(),
        (0..N)
            .map(|i| {
                if i % 2 == 1 {
                    state(0.0)
                } else if (i / 2) % 2 == 0 {
                    state(0.02)
                } else {
                    state(-0.02)
                }
            })
            .collect(),
    );
    states.insert(
        "ETH/USDT".to_string(),
        (0..N)
            .map(|i| {
                if i % 2 == 0 {
                    state(0.0)
                } else if ((i - 1) / 2) % 2 == 0 {
                    state(0.02)
                } else {
                    state(-0.02)
                }
            })
            .collect(),
    );

    let bars = BTreeMap::from([("BTC/USDT".to_string(), btc), ("ETH/USDT".to_string(), eth)]);
    let (a, b) = run_case("case2", &bars, &states, &CaseConfig::default());

    assert_eq!(b.bars_processed, N as u64, "缺口帧也必须触发生命周期");
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

/// 场景 3：保证金 + EOD 强平，验证 margin / force_liquidate 也逐字段一致
#[test]
fn case3_margin_and_force_liquidate() {
    const N: usize = 12;
    let bars = BTreeMap::from([("BTC/USDT".to_string(), zigzag_bars(N, 100.0))]);
    let states = BTreeMap::from([("BTC/USDT".to_string(), state_column(N))]);
    let cfg = CaseConfig {
        initial_cash: 10_000.0,
        force_liquidate: true,
        margin_specs: HashMap::from([("BTC/USDT".to_string(), btc_margin_spec())]),
        futures: Some((SwapSettle::UsdMargin, 1.0)),
    };

    let (a, b) = run_case("case3", &bars, &states, &cfg);

    assert!(a.margin_applied, "A 路应启用保证金账务");
    assert!(b.run.margin_applied, "B 路应启用保证金账务");
    assert_eq!(a.margin_specs_effective.len(), 1, "规格回显应有一个品种");
    assert!(a.fills > 0, "保证金场景也必须真的成交");
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
}
