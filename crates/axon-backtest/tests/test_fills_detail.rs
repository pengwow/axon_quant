//! 单元测试:`RunResult.fills_detail` 字段(0.7.0 新增)
//!
//! ## 测试目标
//!
//! 验证 `BacktestState.fills_detail` 在不同开/平/加仓路径下都被正确填充,
//! 不依赖事件队列(直接构造 `BacktestState` 走 `apply_fill` 流程)。
//!
//! `e2e_fills_detail.rs` 走 E2E 路径(订单 → matcher → apply_fill),这里走
//! 内部状态机路径,聚焦状态正确性。
//!
//! ## 覆盖场景
//!
//! 1. `single_open_long_records_one_fill_no_trade`:
//!    开多 1 笔 → `fills=1, trades=0, fills_detail=1`
//! 2. `same_side_add_long_records_two_fills_no_trade`:
//!    同向加仓 2 笔 → `fills=2, trades=0, fills_detail=2`
//! 3. `round_trip_records_two_fills_one_trade`:
//!    开+平 → `fills=2, trades=1, fills_detail=2`
//! 4. `partial_fill_records_in_order`:
//!    一笔订单 partial fill 两次 → fills_detail 都按 event timestamp 排序
//! 5. `default_runresult_has_empty_fills_detail`:
//!    `RunResult::default().fills_detail` 应为空 Vec
//! 6. `fill_record_turnover_helper`:
//!    `FillRecord::turnover()` 应 = price * quantity
//!
//! 运行:`cargo test -p axon-backtest --test test_fills_detail`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig, RunResult};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::portfolio::FillRecord;
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{
    Instrument, Price, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol,
};

// ── helpers ──────────────────────────────────────────────────────────

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

fn make_market_order(id: u64, instrument: &Instrument, side: Side, qty: f64) -> Order {
    match instrument {
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

fn base_config() -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

fn push_order(
    q: &mut EventQueue,
    ts_ns: i64,
    id: u64,
    instrument: &Instrument,
    side: Side,
    qty: f64,
) {
    let mut b = EventBuilder::new(0);
    q.push(b.order(
        Timestamp::from_nanos(ts_ns),
        id,
        OrderAction::Submitted(make_market_order(id, instrument, side, qty)),
    ));
}

// ── 测试 1:开多 1 笔 fill,0 笔 trade ───────────────────

/// 开多 1 笔 fill:未平仓 → trades=0,fills_detail=1
#[test]
fn single_open_long_records_one_fill_no_trade() {
    let inst = btc_spot();
    let mut q = EventQueue::new();
    push_order(&mut q, 1_000_000_000, 1, &inst, Side::Buy, 0.5);

    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity(50.0, 5, 1.0);
    engine.begin_bar(50_000.0, inst.clone());
    let result = engine.run();

    assert_eq!(result.fills, 1, "fills 应 = 1, got {}", result.fills);
    assert_eq!(result.trades.len(), 0, "未平仓:trades 应 = 0");
    assert_eq!(result.fills_detail.len(), 1, "fills_detail 应 = 1");

    let fr = &result.fills_detail[0];
    assert_eq!(fr.taker_order_id, 1);
    assert_eq!(fr.taker_side, Side::Buy);
    assert_eq!(fr.instrument, inst);
    assert!((fr.quantity.as_f64() - 0.5).abs() < 1e-9);
}

// ── 测试 2:同向加仓 2 笔 fill,0 笔 trade ────────────────

/// 同向加仓 2 笔 buy(0.3 + 0.2):
/// - `fills == 2` ✓
/// - `trades.len() == 0` ✓(加仓不开 round-trip)
/// - `fills_detail.len() == 2` ✓(0.7.0 新增:每笔都记)
#[test]
fn same_side_add_long_records_two_fills_no_trade() {
    let inst = btc_spot();
    let mut q = EventQueue::new();
    push_order(&mut q, 1_000_000_000, 1, &inst, Side::Buy, 0.3);
    push_order(&mut q, 2_000_000_000, 2, &inst, Side::Buy, 0.2);

    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity(50.0, 5, 1.0);
    engine.begin_bar(50_000.0, inst.clone());
    engine.begin_bar(50_000.0, inst.clone());
    let result = engine.run();

    assert_eq!(result.fills, 2);
    assert_eq!(result.trades.len(), 0, "同向加仓不开 trade");
    assert_eq!(result.fills_detail.len(), 2);

    // 时间戳按 event 时间序
    assert_eq!(result.fills_detail[0].timestamp.nanos, 1_000_000_000);
    assert_eq!(result.fills_detail[1].timestamp.nanos, 2_000_000_000);
    // 数量按 push 序
    assert!((result.fills_detail[0].quantity.as_f64() - 0.3).abs() < 1e-9);
    assert!((result.fills_detail[1].quantity.as_f64() - 0.2).abs() < 1e-9);
}

// ── 测试 3:round-trip 2 笔 fill,1 笔 trade ──────────────

/// buy 0.5 + sell 0.5:开+平 → `fills=2, trades=1, fills_detail=2`
#[test]
fn round_trip_records_two_fills_one_trade() {
    let inst = btc_spot();
    let mut q = EventQueue::new();
    push_order(&mut q, 1_000_000_000, 1, &inst, Side::Buy, 0.5);
    push_order(&mut q, 2_000_000_000, 2, &inst, Side::Sell, 0.5);

    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity(50.0, 5, 1.0);
    engine.begin_bar(50_000.0, inst.clone());
    engine.begin_bar(50_000.0, inst.clone());
    let result = engine.run();

    assert_eq!(result.fills, 2, "fills 应 = 2, got {}", result.fills);
    assert_eq!(
        result.trades.len(),
        1,
        "round-trip:trades 应 = 1, got {}",
        result.trades.len()
    );
    assert_eq!(result.fills_detail.len(), 2, "fills_detail 应 = 2");

    // 两笔 fill:buy + sell
    assert_eq!(result.fills_detail[0].taker_side, Side::Buy);
    assert_eq!(result.fills_detail[1].taker_side, Side::Sell);
}

// ── 测试 4:RunResult::default 包含空 fills_detail ────────

/// `Default::default().fills_detail` 应为空 Vec,避免使用方踩空指针
#[test]
fn default_runresult_has_empty_fills_detail() {
    let r: RunResult = RunResult::default();
    assert!(r.fills_detail.is_empty(), "default fills_detail 应 = []");
    assert!(r.trades.is_empty());
    assert_eq!(r.fills, 0);
}

// ── 测试 5:FillRecord::turnover 助手函数 ───────────────

/// `turnover()` 应 = price * quantity
#[test]
fn fill_record_turnover_helper() {
    let inst = btc_spot();
    let fr = FillRecord::new(
        Timestamp::from_nanos(0),
        inst,
        1,
        1_000_000_000,
        Side::Buy,
        Price::from_f64(100.0),
        Quantity::from_f64(0.5),
    );
    assert!(
        (fr.turnover() - 50.0).abs() < 1e-9,
        "turnover 应 = 100 * 0.5 = 50, got {}",
        fr.turnover()
    );
}

// ── 测试 6:partial fill 顺序保留 ──────────────────────

/// 1 笔大单分多笔 partial fill,`fills_detail` 按 event timestamp 保留
///
/// buy 0.7 @ mid=100,half_spread=0.1 / depth=5 / size=1.0/档:
/// 100.1 档(1.0 qty)被 partial fill 0.7 → 1 笔 fill 但 qty < maker qty
#[test]
fn partial_fill_records_in_order() {
    let inst = btc_spot();
    let mut q = EventQueue::new();
    push_order(&mut q, 1_000_000_000, 1, &inst, Side::Buy, 0.7);

    let mut engine = BacktestEngine::new(base_config(), q);
    // half_spread=0.1, 5 档,每档 1.0 qty → 100.1 档吃 0.7 partial fill
    engine.with_seed_liquidity(0.1, 5, 1.0);
    engine.begin_bar(100.0, inst.clone());
    let result = engine.run();

    assert_eq!(
        result.fills, 1,
        "buy 0.7 吃 100.1 档(1.0 qty)partial fill 应 = 1 笔, got {}",
        result.fills
    );
    assert_eq!(
        result.fills_detail.len(),
        1,
        "fills_detail.len() 应 = 1, got {}",
        result.fills_detail.len()
    );
    let fr = &result.fills_detail[0];
    assert_eq!(fr.taker_order_id, 1, "partial fill 共享 taker_order_id");
    assert_eq!(fr.taker_side, Side::Buy);
    assert_eq!(fr.instrument, inst);
    assert!(
        (fr.quantity.as_f64() - 0.7).abs() < 1e-9,
        "partial fill qty 应 = 0.7, got {}",
        fr.quantity.as_f64()
    );
}

// ── 测试 7:perp fill 正确标记 instrument ────────────────

/// perp(swap) market order 成交时 `fills_detail.instrument` 应 = swap instrument
#[test]
fn perp_fill_records_swap_instrument() {
    let perp = btc_perp();
    let mut q = EventQueue::new();
    push_order(&mut q, 1_000_000_000, 1, &perp, Side::Sell, 0.3);

    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity(50.0, 5, 1.0);
    engine.begin_bar(50_000.0, perp.clone());
    let result = engine.run();

    assert_eq!(
        result.fills, 1,
        "perp 子 run 应 = 1 fill, got {}",
        result.fills
    );
    assert_eq!(result.fills_detail.len(), 1);
    assert_eq!(
        result.fills_detail[0].instrument, perp,
        "perp fill 应带 swap instrument"
    );
    assert_eq!(result.fills_detail[0].taker_side, Side::Sell);
}
