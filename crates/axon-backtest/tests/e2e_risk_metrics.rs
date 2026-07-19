//! Phase 4.5 端到端测试:`RunResult.risk_metrics` 暴露与正确性
//!
//! 目标:
//! 1. 验证 `BacktestEngine::run()` 后 `RunResult.risk_metrics` 已填充
//! 2. spot + perp delta-neutral 场景,`portfolio_delta ≈ 0`
//! 3. per-leg delta 字段精确(spot +1 → +1;perp -1 → -1)
//! 4. total_gamma / vega / per_leg_gamma 暂时全 0
//! 5. sharpe_with_legs 沿用 `sharpe_ratio`
//!
//! 运行:`cargo test -p axon-backtest --test e2e_risk_metrics`

use std::collections::HashMap;

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol};

// ─── helpers ───────────────────────────────────────────────

fn btc_spot() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

fn btc_perp() -> Instrument {
    Instrument::Swap(SwapInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
        settle: SwapSettle::UsdMargin,
        contract_size: 1.0,
    })
}

/// Market + IOC 单(撮合引擎即时成交,需 seed 对手盘)
fn make_market(id: u64, inst: &Instrument, side: Side, qty: f64) -> Order {
    match inst {
        Instrument::Spot(s) => Order::spot(
            id,
            s.base.clone(),
            s.quote.clone(),
            side,
            OrderType::Market,
            Quantity::from_f64(qty),
            TimeInForce::IOC,
        ),
        Instrument::Swap(s) => Order::swap(
            id,
            s.base.clone(),
            s.quote.clone(),
            s.settle,
            s.contract_size,
            side,
            OrderType::Market,
            Quantity::from_f64(qty),
            TimeInForce::IOC,
        ),
    }
}

/// 构造带 seed_liquidity 的 spot@100 + perp@100.5 撮合环境
fn new_engine_seeded() -> BacktestEngine {
    let spot = btc_spot();
    let perp = btc_perp();
    let config = BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    };
    let mut engine = BacktestEngine::new(config, EventQueue::new());
    // seed spot + perp 各 5 档 × 1.0 size
    engine.with_seed_liquidity(50.0, 5, 1.0);
    engine.begin_bar_multi(vec![(spot, 50_000.0), (perp, 50_000.0)]);
    engine
}

// ═════════════════════════════════════════════════════════════
// E2E 1: 空回测 → risk_metrics 全 0
// ═════════════════════════════════════════════════════════════

#[test]
fn empty_run_risk_metrics_zero() {
    let config = BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    };
    let mut engine = BacktestEngine::new(config, EventQueue::new());
    let result = engine.run();
    assert_eq!(result.fills, 0);
    assert!(result.positions.is_empty());
    let rm = &result.risk_metrics;
    assert!(rm.per_leg_delta.is_empty(), "无持仓 → 无 delta 暴露");
    assert_eq!(rm.portfolio_delta, 0.0);
    assert!(rm.per_leg_gamma.is_empty());
    assert_eq!(rm.total_gamma, 0.0);
    assert_eq!(rm.vega, 0.0);
    assert_eq!(rm.sharpe_with_legs, 0.0);
}

// ═════════════════════════════════════════════════════════════
// E2E 2: 单 leg buy market 1 BTC → portfolio_delta = 1
// ═════════════════════════════════════════════════════════════

#[test]
fn single_leg_long_1_btc_delta_is_one() {
    let mut engine = new_engine_seeded();
    let inst = btc_spot();
    let mut b = EventBuilder::new(0);
    let ts = Timestamp::from_nanos(1_000);
    let order = make_market(1, &inst, Side::Buy, 1.0);
    engine.push_event(b.order(ts, 1, OrderAction::Submitted(order)));

    let result = engine.run();
    let rm = &result.risk_metrics;
    assert_eq!(rm.per_leg_delta.len(), 1, "1 笔持仓");
    assert!(
        (rm.per_leg_delta[&inst] - 1.0).abs() < 1e-9,
        "1 BTC long → delta = +1"
    );
    assert!((rm.portfolio_delta - 1.0).abs() < 1e-9);
    assert!(rm.per_leg_gamma.contains_key(&inst));
    assert_eq!(rm.per_leg_gamma[&inst], 0.0);
    assert_eq!(rm.total_gamma, 0.0);
    assert_eq!(rm.vega, 0.0);
}

// ═════════════════════════════════════════════════════════════
// E2E 3: spot + perp delta-neutral → portfolio_delta ≈ 0
// ═════════════════════════════════════════════════════════════

#[test]
fn spot_perp_delta_neutral_total_zero() {
    let mut engine = new_engine_seeded();
    let spot = btc_spot();
    let perp = btc_perp();
    let mut b = EventBuilder::new(0);
    let ts1 = Timestamp::from_nanos(1_000);
    let ts2 = Timestamp::from_nanos(2_000);

    // spot buy 1 BTC
    let o1 = make_market(1, &spot, Side::Buy, 1.0);
    engine.push_event(b.order(ts1, 1, OrderAction::Submitted(o1)));
    // perp sell 1 BTC
    let o2 = make_market(2, &perp, Side::Sell, 1.0);
    engine.push_event(b.order(ts2, 2, OrderAction::Submitted(o2)));

    let result = engine.run();
    let rm = &result.risk_metrics;
    assert_eq!(rm.per_leg_delta.len(), 2, "spot + perp 各 1 笔");
    assert!((rm.per_leg_delta[&spot] - 1.0).abs() < 1e-9);
    assert!((rm.per_leg_delta[&perp] - (-1.0)).abs() < 1e-9);
    assert!(
        (rm.portfolio_delta - 0.0).abs() < 1e-9,
        "delta-neutral: portfolio_delta = 0"
    );
}

// ═════════════════════════════════════════════════════════════
// E2E 4: 多 leg 净 delta = 0.5
// ═════════════════════════════════════════════════════════════

#[test]
fn multi_leg_aggregation_total_delta() {
    let mut engine = new_engine_seeded();
    let spot = btc_spot();
    let perp = btc_perp();
    let mut b = EventBuilder::new(0);
    let ts1 = Timestamp::from_nanos(1_000);
    let ts2 = Timestamp::from_nanos(2_000);

    let o1 = make_market(1, &spot, Side::Buy, 1.0);
    engine.push_event(b.order(ts1, 1, OrderAction::Submitted(o1)));
    let o2 = make_market(2, &perp, Side::Sell, 0.5);
    engine.push_event(b.order(ts2, 2, OrderAction::Submitted(o2)));

    let result = engine.run();
    let rm = &result.risk_metrics;
    assert!((rm.portfolio_delta - 0.5).abs() < 1e-9);
}

// ═════════════════════════════════════════════════════════════
// E2E 5: sharpe_with_legs 沿用 sharpe_ratio
// ═════════════════════════════════════════════════════════════

#[test]
fn sharpe_with_legs_matches_sharpe_ratio() {
    let mut engine = new_engine_seeded();
    let inst = btc_spot();
    let mut b = EventBuilder::new(0);
    let ts = Timestamp::from_nanos(1_000);
    let order = make_market(1, &inst, Side::Buy, 1.0);
    engine.push_event(b.order(ts, 1, OrderAction::Submitted(order)));

    let result = engine.run();
    assert_eq!(
        result.risk_metrics.sharpe_with_legs, result.sharpe_ratio,
        "sharpe_with_legs 必须 == sharpe_ratio(0.7.0 范围)"
    );
}

// ═════════════════════════════════════════════════════════════
// E2E 6: RunResult::default().risk_metrics 全空
// ═════════════════════════════════════════════════════════════

#[test]
fn run_result_default_risk_metrics_empty() {
    let result = axon_backtest::engine::RunResult::default();
    let rm = &result.risk_metrics;
    assert!(rm.per_leg_delta.is_empty());
    assert_eq!(rm.portfolio_delta, 0.0);
    assert!(rm.per_leg_gamma.is_empty());
    assert_eq!(rm.total_gamma, 0.0);
    assert_eq!(rm.vega, 0.0);
    assert_eq!(rm.sharpe_with_legs, 0.0);
}

// ═════════════════════════════════════════════════════════════
// E2E 7: from_positions helper 正确性
// ═════════════════════════════════════════════════════════════

#[test]
fn risk_metrics_report_from_positions_correctness() {
    let mut positions = HashMap::new();
    positions.insert(btc_spot(), 1.0);
    positions.insert(btc_perp(), -0.5);
    let report = axon_backtest::engine::RiskMetricsReport::from_positions(&positions, 1.5);
    assert_eq!(report.per_leg_delta.len(), 2);
    assert!((report.per_leg_delta[&btc_spot()] - 1.0).abs() < 1e-9);
    assert!((report.per_leg_delta[&btc_perp()] - (-0.5)).abs() < 1e-9);
    assert!((report.portfolio_delta - 0.5).abs() < 1e-9);
    assert_eq!(report.per_leg_gamma.len(), 2);
    assert!(report.per_leg_gamma.values().all(|&v| v == 0.0));
    assert_eq!(report.sharpe_with_legs, 1.5);
}
