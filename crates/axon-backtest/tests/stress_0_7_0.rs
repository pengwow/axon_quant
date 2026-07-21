//! 0.7.0 压力 + 边界测试
//!
//! 找隐藏的 bug / 性能问题 / 内存问题
//!
//! 运行:`cargo test -p axon-backtest --test stress_0_7_0 -- --nocapture`

use std::time::Instant;

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_backtest::matching::l3::book::L3Book;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol};

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

// ═════════════════════════════════════════════════════════════
// 压力 1: 1 万 fill 的 fills_detail 内存 + 顺序
// ═════════════════════════════════════════════════════════════

#[test]
fn stress_10k_fills_memory_and_order() {
    let inst = btc_spot();
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    for i in 0..10_000u64 {
        let order = make_market(i + 1, &inst, Side::Buy, 0.001);
        q.push(b.order(
            Timestamp::from_nanos(((i + 1) * 1_000) as i64),
            i + 1,
            OrderAction::Submitted(order),
        ));
    }
    let mut engine = BacktestEngine::new(base_config(), q);
    // size_per_level=1.0 → 10 档 × 1.0 = 10 总卖量,足够 10K 笔 0.001 buy 全部 fill
    engine.with_seed_liquidity(50.0, 10, 1.0);
    engine.begin_bar(50_000.0, inst.clone());

    let start = Instant::now();
    let result = engine.run();
    let elapsed = start.elapsed();

    // 至少 10K 笔 fill(seed 总量 10 足够 10K 笔 0.001;允许因 path 差异
    // 多 10-20 笔,核心是验证内存 + 时间戳单调)
    assert!(
        result.fills >= 10_000,
        "应 ≥10000 笔 fill, got {}",
        result.fills
    );
    assert!(
        result.fills_detail.len() as u64 == result.fills,
        "fills_detail 数应与 fills 一致, fills={} fills_detail={}",
        result.fills,
        result.fills_detail.len()
    );
    // 时间戳应严格单调递增
    for i in 1..result.fills_detail.len() {
        let prev = result.fills_detail[i - 1].timestamp.nanos;
        let curr = result.fills_detail[i].timestamp.nanos;
        assert!(
            curr >= prev,
            "fill 时间戳应单调:prev={} curr={} (i={})",
            prev,
            curr,
            i
        );
    }
    println!("{} fills in {:?}, fills_detail OK", result.fills, elapsed);
}

// ═════════════════════════════════════════════════════════════
// 压力 2: L3Book 在 deep book(100 价位 × 100 单)下的性能
// ═════════════════════════════════════════════════════════════

#[test]
fn stress_l3book_deep_book_perf() {
    use axon_backtest::matching::engine::L1MatchingEngine;
    let mut engine = L1MatchingEngine::new();
    let inst = btc_spot();

    // seed 100 档 × 100 单 = 10K 限价单
    let start_seed = Instant::now();
    let _ = engine.seed_liquidity(100.0, 0.5, 100, 1.0, inst.clone(), 1_000_000_000);
    let seed_elapsed = start_seed.elapsed();

    // 转 L3Book
    let start_l3 = Instant::now();
    let book = L3Book::from_l1_engine_for(&engine, &inst);
    let l3_elapsed = start_l3.elapsed();

    let total_orders = book.total_bid_orders() + book.total_ask_orders();
    println!(
        "seed 100x100 in {:?}, L3Book in {:?}, orders: {}",
        seed_elapsed, l3_elapsed, total_orders
    );

    assert!(total_orders > 0);
    assert!(book.best_bid().is_some());
    assert!(book.best_ask().is_some());
    // 序列化 round-trip
    let json = serde_json::to_string(&book).unwrap();
    println!("L3Book JSON size: {} bytes", json.len());
    assert!(!json.is_empty());
}

// ═════════════════════════════════════════════════════════════
// 边界 1: empty run 状态一致性
// ═════════════════════════════════════════════════════════════

#[test]
fn edge_empty_run_consistency() {
    let mut engine = BacktestEngine::new(base_config(), EventQueue::new());
    // 没调 with_seed_liquidity,没 begin_bar,没 push_event
    let result = engine.run();
    assert_eq!(result.fills, 0);
    assert!(result.fills_detail.is_empty());
    assert!(result.positions.is_empty());
    assert!(result.trades.is_empty());
    assert!(result.fills_detail.is_empty());

    let rm = &result.risk_metrics;
    assert_eq!(rm.portfolio_delta, 0.0);
    assert_eq!(rm.total_gamma, 0.0);
    assert_eq!(rm.vega, 0.0);
    // sharpe_with_legs = sharpe_ratio (0.7.0 范围)
    assert_eq!(rm.sharpe_with_legs, result.sharpe_ratio);
}

// ═════════════════════════════════════════════════════════════
// 边界 2: 单根 bar 跨多次 seed_liquidity(同 instrument)
// ═════════════════════════════════════════════════════════════

#[test]
fn edge_repeated_seed_liquidity_same_instrument() {
    let inst = btc_spot();
    let mut engine = BacktestEngine::new(base_config(), EventQueue::new());
    engine.with_seed_liquidity(50.0, 5, 1.0);

    // 1 根 bar 调 3 次 begin_bar(同 instrument)
    engine.begin_bar(50_000.0, inst.clone());
    engine.begin_bar(50_000.0, inst.clone());
    engine.begin_bar(50_000.0, inst.clone());

    // bar 1 + 2 + 3,都吃同样的 maker
    let mut b = EventBuilder::new(0);
    engine.push_event(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_market(1, &inst, Side::Buy, 0.1)),
    ));
    let result = engine.run();
    println!(
        "repeated seed: fills={} fills_detail={}",
        result.fills,
        result.fills_detail.len()
    );
    // 3 次 seed,bar_id 自增 3,rebalance 3 次,每次 maker 都被 clear+seed
    // fill 至少 1
    assert!(result.fills >= 1);
}

// ═════════════════════════════════════════════════════════════
// 边界 3: begin_bar_multi vs 多次 begin_bar 行为差异
// ═════════════════════════════════════════════════════════════

#[test]
fn edge_begin_bar_multi_vs_loop_behavior() {
    // 场景 1: 1 次 begin_bar_multi(spot + perp)
    let mut e1 = BacktestEngine::new(base_config(), EventQueue::new());
    e1.with_seed_liquidity(50.0, 5, 1.0);
    e1.begin_bar_multi(vec![(btc_spot(), 50_000.0), (btc_perp(), 50_010.0)]);
    let r1 = e1.run();
    println!("begin_bar_multi: fills={} bar_id implicit", r1.fills);

    // 场景 2: 2 次 begin_bar(同效果)
    let mut e2 = BacktestEngine::new(base_config(), EventQueue::new());
    e2.with_seed_liquidity(50.0, 5, 1.0);
    e2.begin_bar(50_000.0, btc_spot());
    e2.begin_bar(50_010.0, btc_perp());
    let r2 = e2.run();
    println!("2x begin_bar: fills={} bar_id increments 2x", r2.fills);
}
