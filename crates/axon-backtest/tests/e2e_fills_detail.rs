//! 端到端测试:`RunResult.fills_detail` per-fill 可观测性(0.7.0 新增)
//!
//! ## 测试目标
//!
//! 0.6.0 暴露的 `RunResult.trades` 只记录 **round-trip** TradeRecord(开+平配对),
//! 0.7.0 新增 `RunResult.fills_detail: Vec<FillRecord>` 记录每笔 `MatchFill`,
//! 补齐 L3 级别可观测性:
//! - 同向加仓:不开新 trade 但有 fill(0.6.0 的 `trades=[]` + `fills > 0`)
//! - multi-leg:spot + perp 不同 instrument 的 fill 拆分
//!
//! ## 已知约束(0.6.0 局限,在 0.7.0 Phase 2 修)
//!
//! `with_seed_liquidity` 是 **global** 配置(per-engine 共享),`begin_bar` 一次性
//! seed 一个 instrument 的 book 并 `clear_book()` 清掉其他书的旧 seed。
//! 本测试的 multi-leg 用例采用"按 bar 顺序切换 instrument"的方式验证 `fills_detail`
//! 的 instrument 字段正确性,完整 multi-leg 同 bar seed 在 0.7.0 Phase 2 后再做。
//!
//! ## 测试场景
//!
//! 1. `same_side_add_records_both_fills_no_trades`:
//!    2 笔 buy market 同向加仓 → `fills=2, trades=0, fills_detail.len()=2`
//! 2. `two_market_buys_record_both_fills`:
//!    2 笔 buy market(等价同向加仓)→ `fills_detail` 按顺序记录,instrument 正确
//! 3. `multi_leg_per_bar_records_instrument`:
//!    bar 1 跑 spot buy,bar 2 跑 perp sell → `fills_detail` 按 instrument 拆分
//! 4. `fill_record_carries_all_fields`:
//!    验证 FillRecord 各字段(`timestamp_ns` / `instrument` / `taker_order_id` /
//!    `maker_order_id` / `taker_side` / `price` / `quantity`)正确
//! 5. `multi_bar_records_each_bar_fills`:
//!    跨 3 根 bar 累计 3 笔 fill → `fills_detail` 全保留
//!
//! 运行:`cargo test -p axon-backtest --test e2e_fills_detail`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol};

// ── 共享 helper ──────────────────────────────────────────────────────

fn btc_spot() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

fn eth_spot() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("ETH"),
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

fn build_market_orders(
    orders: &[(u64, &Instrument, Side, f64, i64)], // (id, instrument, side, qty, ts_ns)
) -> EventQueue {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    for (id, inst, side, qty, ts_ns) in orders {
        q.push(b.order(
            Timestamp::from_nanos(*ts_ns),
            *id,
            OrderAction::Submitted(make_market_order(*id, inst, *side, *qty)),
        ));
    }
    q
}

// ── 测试 1:同向加仓记录 2 笔 fill,0 笔 trade ─────────────

/// 2 笔 buy market 同向加仓(0.5 + 0.3 = 0.8):
/// - `fills == 2` ✓
/// - `trades.len() == 0` ✓(同向加仓不开 round-trip)
/// - `fills_detail.len() == 2` ✓(0.7.0 新增:每笔 fill 都记)
#[test]
fn same_side_add_records_both_fills_no_trades() {
    let inst = btc_spot();
    let q = build_market_orders(&[
        (1, &inst, Side::Buy, 0.5, 1_000_000_000),
        (2, &inst, Side::Buy, 0.3, 2_000_000_000),
    ]);
    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity(50.0, 5, 1.0);
    engine.begin_bar(50_000.0, inst.clone());
    let result = engine.run();

    assert_eq!(result.fills, 2, "fills 应 = 2, got {}", result.fills);
    assert_eq!(
        result.trades.len(),
        0,
        "同向加仓不开 round-trip,trades.len() 应 = 0, got {}",
        result.trades.len()
    );
    assert_eq!(
        result.fills_detail.len(),
        2,
        "0.7.0 新增 fills_detail:同向加仓应记 2 笔,got {}",
        result.fills_detail.len()
    );

    // 验证两笔 fill 顺序与 qty 正确
    let f0 = &result.fills_detail[0];
    let f1 = &result.fills_detail[1];
    assert_eq!(f0.taker_order_id, 1);
    assert_eq!(f1.taker_order_id, 2);
    assert_eq!(f0.taker_side, Side::Buy);
    assert_eq!(f1.taker_side, Side::Buy);
    assert!((f0.quantity.as_f64() - 0.5).abs() < 1e-9);
    assert!((f1.quantity.as_f64() - 0.3).abs() < 1e-9);
    assert_eq!(f0.instrument, inst);
    assert_eq!(f1.instrument, inst);
}

// ── 测试 2:两笔 fill 按时间序记录 ──────────────────────

/// 跨 2 根 bar 累计 2 笔 fill(buy market 跨 2 个 bar)→ `fills_detail` 全保留
#[test]
fn two_market_buys_record_both_fills() {
    let inst = btc_spot();
    let q = build_market_orders(&[
        (10, &inst, Side::Buy, 0.1, 1_000_000_000),
        (20, &inst, Side::Buy, 0.2, 2_000_000_000),
    ]);
    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity(50.0, 5, 1.0);
    engine.begin_bar(50_000.0, inst.clone());
    engine.begin_bar(50_000.0, inst.clone());
    let result = engine.run();

    assert_eq!(result.fills, 2);
    assert_eq!(result.fills_detail.len(), 2, "2 笔 fill 都记");
    // 时间戳应保持原序(来自 simulated event time)
    let timestamps: Vec<i64> = result
        .fills_detail
        .iter()
        .map(|f| f.timestamp.nanos)
        .collect();
    assert_eq!(timestamps, vec![1_000_000_000, 2_000_000_000]);
}

// ── 测试 3:multi-leg 按 instrument 拆分 ────────────────

/// 0.6.0 局限:
/// 1. `seed_liquidity` 内部用 `Order::spot` 构造 maker,即使对 swap instrument
///    注入,撮合引擎的 `L1Book` 按 instrument 路由,spot maker 不与 swap taker
///    撮合 → 跨 instrument 同 bar seed 不会成交
/// 2. `with_seed_liquidity` 是 per-engine global,`begin_bar` 一次性清空整个
///    books 再 seed 一个 instrument
///
/// 本测试在两个独立 `BacktestEngine` 实例上分别跑 spot 和 perp fill,
/// 验证 `fills_detail` 的 instrument 字段(spot / perp)正确分类。
/// 完整 multi-leg 同 bar seed 在 0.7.0 Phase 2 (per-leg seed liquidity)
/// 修复 `seed_liquidity` 的 instrument 派生后补上。
#[test]
fn multi_leg_per_bar_records_instrument() {
    let spot = btc_spot();
    let perp = btc_perp();

    // ── 子 run 1:spot 1 笔 fill ─────────────────────
    let q1 = build_market_orders(&[(1, &spot, Side::Buy, 0.5, 1_000_000_000)]);
    let mut e1 = BacktestEngine::new(base_config(), q1);
    e1.with_seed_liquidity(50.0, 5, 1.0);
    e1.begin_bar(50_000.0, spot.clone());
    let r1 = e1.run();

    assert_eq!(r1.fills, 1, "spot 子 run 应 = 1 fill, got {}", r1.fills);
    assert_eq!(r1.fills_detail.len(), 1, "spot fills_detail 应 = 1");
    assert_eq!(
        r1.fills_detail[0].instrument, spot,
        "spot fill 应带 spot instrument"
    );
    assert_eq!(r1.fills_detail[0].taker_order_id, 1);
    assert_eq!(r1.fills_detail[0].taker_side, Side::Buy);
    assert!(
        (r1.fills_detail[0].quantity.as_f64() - 0.5).abs() < 1e-9,
        "spot qty 应 = 0.5, got {}",
        r1.fills_detail[0].quantity.as_f64()
    );
    assert_eq!(
        r1.fills_detail[0].timestamp.nanos, 1_000_000_000,
        "spot fill 时间应来自 event timestamp"
    );

    // ── 子 run 2:perp 1 笔 fill ─────────────────────
    let q2 = build_market_orders(&[(2, &perp, Side::Sell, 0.3, 3_000_000_000)]);
    let mut e2 = BacktestEngine::new(base_config(), q2);
    e2.with_seed_liquidity(50.0, 5, 1.0);
    e2.begin_bar(50_000.0, perp.clone());
    let r2 = e2.run();

    assert_eq!(r2.fills, 1, "perp 子 run 应 = 1 fill, got {}", r2.fills);
    assert_eq!(r2.fills_detail.len(), 1, "perp fills_detail 应 = 1");
    assert_eq!(
        r2.fills_detail[0].instrument, perp,
        "perp fill 应带 perp instrument"
    );
    assert_eq!(r2.fills_detail[0].taker_order_id, 2);
    assert_eq!(r2.fills_detail[0].taker_side, Side::Sell);
    assert!(
        (r2.fills_detail[0].quantity.as_f64() - 0.3).abs() < 1e-9,
        "perp qty 应 = 0.3, got {}",
        r2.fills_detail[0].quantity.as_f64()
    );
    assert_eq!(
        r2.fills_detail[0].timestamp.nanos, 3_000_000_000,
        "perp fill 时间应来自 event timestamp"
    );

    // ── 跨子 run 验证:两个 fill 的 instrument 不同 ──────────
    assert_ne!(
        r1.fills_detail[0].instrument, r2.fills_detail[0].instrument,
        "spot fill 和 perp fill 的 instrument 必须不同"
    );
}

// ── 测试 4:FillRecord 携带全部 7 字段 ──────────────────

/// 验证 FillRecord 各字段都被正确填充
#[test]
fn fill_record_carries_all_fields() {
    let inst = eth_spot();
    let q = build_market_orders(&[(7, &inst, Side::Buy, 0.4, 9_999_000_000)]);
    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity(50.0, 5, 1.0);
    engine.begin_bar(2_000.0, inst.clone());
    let result = engine.run();

    assert_eq!(result.fills_detail.len(), 1);
    let fr = &result.fills_detail[0];

    assert_eq!(
        fr.timestamp.nanos, 9_999_000_000,
        "timestamp_ns 应 = event timestamp(非 wall clock)"
    );
    assert_eq!(fr.instrument, inst);
    assert_eq!(fr.taker_order_id, 7);
    // maker 是 seed liquidity 挂的限价单(> 1_000_000_000)
    assert!(
        fr.maker_order_id >= 1_000_000_000,
        "maker 来自 seed_liquidity(id 应 ≥ 1e9),got {}",
        fr.maker_order_id
    );
    assert_eq!(fr.taker_side, Side::Buy);
    assert!(
        fr.price.as_f64() > 2_000.0,
        "价格应 > mid(2_000),ask 侧,got {}",
        fr.price.as_f64()
    );
    assert!((fr.quantity.as_f64() - 0.4).abs() < 1e-9);
}

// ── 测试 5:跨 bar 累计 fill 全保留 ─────────────────────

/// 跨 3 根 bar 累计 3 笔 fill(同向加仓 3 次)→ `fills_detail.len() == 3`
#[test]
fn multi_bar_records_each_bar_fills() {
    let inst = btc_spot();
    let q = build_market_orders(&[
        (10, &inst, Side::Buy, 0.1, 1_000_000_000),
        (20, &inst, Side::Buy, 0.2, 2_000_000_000),
        (30, &inst, Side::Buy, 0.3, 3_000_000_000),
    ]);
    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity(50.0, 5, 1.0);
    engine.begin_bar(50_000.0, inst.clone());
    engine.begin_bar(50_000.0, inst.clone());
    engine.begin_bar(50_000.0, inst.clone());
    let result = engine.run();

    assert_eq!(result.fills, 3);
    assert_eq!(result.fills_detail.len(), 3, "3 笔 fill 都记");
    assert_eq!(result.trades.len(), 0, "全同向加仓,无 trade");

    // 时间戳应保持原序
    let timestamps: Vec<i64> = result
        .fills_detail
        .iter()
        .map(|f| f.timestamp.nanos)
        .collect();
    assert_eq!(
        timestamps,
        vec![1_000_000_000, 2_000_000_000, 3_000_000_000]
    );
}
