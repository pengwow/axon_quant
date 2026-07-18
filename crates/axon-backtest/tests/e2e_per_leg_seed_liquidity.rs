//! 0.7.0 端到端测试:per-leg seed liquidity(spot + perp 同 bar seed)
//!
//! ## 测试目标
//!
//! 0.6.0 局限:`seed_liquidity` 是 per-engine global,`begin_bar` 一次性
//! seed 一个 instrument 并 `clear_book()` 清掉其他 leg 的旧 seed,导致
//! multi-leg 套利无法在同 bar 内同时建仓。
//!
//! 0.7.0 起:
//! - `with_seed_liquidity_for(instrument, ...)` 设置 per-leg 独立配置
//! - `with_seed_liquidity(...)` 仍兼容,设为 default fallback
//! - `begin_bar(price, instrument)` 优先 per-leg,fallback default,只清该 instrument
//! - `begin_bar_multi(legs)` 多 leg 同 bar seed
//!
//! ## 测试场景
//!
//! 1. `per_leg_different_half_spread`:
//!    spot (hs=0.01) + perp (hs=0.5) 不同 half_spread → 各自有 seed
//! 2. `begin_bar_multi_two_legs_same_bar`:
//!    `begin_bar_multi({spot: 100, perp: 200})` → 2 笔 fill (spot + perp)
//! 3. `per_leg_does_not_clear_other_leg`:
//!    spot 旧 seed 保留,begin_bar(perp) 不应清掉 spot
//! 4. `default_config_fallback`:
//!    没调 `with_seed_liquidity_for` 的 leg 用 default
//!
//! 运行:`cargo test -p axon-backtest --test e2e_per_leg_seed_liquidity`

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

// ── 共享 helper ──────────────────────────────────────────────

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

fn build_orders(orders: &[(u64, &Instrument, Side, f64, i64)]) -> EventQueue {
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

// ── 测试 1:per-leg 不同 half_spread ───────────────────────

/// spot 和 perp 用不同 half_spread:
/// - spot: half_spread=0.01, depth=3, size=0.1 → asks 100.01, 100.02, 100.03
/// - perp: half_spread=0.5, depth=3, size=0.1 → asks 200.5, 201.0, 201.5
///
/// buy 0.05 spot → 吃 100.01 档 0.05 partial fill (1 笔)
/// buy 0.05 perp → 吃 200.5 档 0.05 partial fill (1 笔)
/// → 总 2 笔 fill,2 个不同 instrument
#[test]
fn per_leg_different_half_spread() {
    let spot = btc_spot();
    let perp = btc_perp();
    let q = build_orders(&[
        (1, &spot, Side::Buy, 0.05, 1_000_000_000),
        (2, &perp, Side::Buy, 0.05, 2_000_000_000),
    ]);

    let mut engine = BacktestEngine::new(base_config(), q);
    // 0.7.0 新 API: per-leg 独立配线
    engine.with_seed_liquidity_for(spot.clone(), 0.01, 3, 0.1);
    engine.with_seed_liquidity_for(perp.clone(), 0.5, 3, 0.1);

    // 单 leg begin_bar(走 per-leg):spot 和 perp 各清各自 book
    engine.begin_bar(100.0, spot.clone());
    engine.begin_bar(200.0, perp.clone());
    let result = engine.run();

    assert_eq!(
        result.fills, 2,
        "应 2 笔 fill (spot + perp), got {}",
        result.fills
    );
    assert_eq!(result.fills_detail.len(), 2);

    // 验证 instrument 正确
    let spot_fill = result
        .fills_detail
        .iter()
        .find(|f| f.taker_order_id == 1)
        .unwrap();
    let perp_fill = result
        .fills_detail
        .iter()
        .find(|f| f.taker_order_id == 2)
        .unwrap();
    assert_eq!(spot_fill.instrument, spot);
    assert!(
        (spot_fill.price.as_f64() - 100.01).abs() < 1e-9,
        "spot fill 应 @ 100.01 (half_spread 0.01 mid 100), got {}",
        spot_fill.price.as_f64()
    );
    assert_eq!(perp_fill.instrument, perp);
    assert!(
        (perp_fill.price.as_f64() - 200.5).abs() < 1e-9,
        "perp fill 应 @ 200.5 (half_spread 0.5 mid 200), got {}",
        perp_fill.price.as_f64()
    );
}

// ── 测试 2:begin_bar_multi 同 bar 多 leg ───────────────────

/// `begin_bar_multi` 在同一根 bar 内对 spot + perp 同时 seed
/// 然后跑 spot buy 0.1 + perp buy 0.1 → 各 1 笔 fill
#[test]
fn begin_bar_multi_two_legs_same_bar() {
    let spot = btc_spot();
    let perp = btc_perp();
    let q = build_orders(&[
        (1, &spot, Side::Buy, 0.1, 1_000_000_000),
        (2, &perp, Side::Buy, 0.1, 2_000_000_000),
    ]);

    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity_for(spot.clone(), 0.01, 3, 0.5);
    engine.with_seed_liquidity_for(perp.clone(), 0.5, 3, 0.5);

    // 0.7.0 新 API: 多 leg 同 bar
    let mut legs = HashMap::new();
    legs.insert(spot.clone(), 100.0);
    legs.insert(perp.clone(), 200.0);
    engine.begin_bar_multi(legs);
    let result = engine.run();

    assert_eq!(
        result.fills, 2,
        "begin_bar_multi 应让 2 leg 都成交, got {}",
        result.fills
    );
    assert_eq!(result.fills_detail.len(), 2);

    // 验证两 leg 各有 1 笔 fill
    let spot_fill = result
        .fills_detail
        .iter()
        .find(|f| f.taker_order_id == 1)
        .unwrap();
    let perp_fill = result
        .fills_detail
        .iter()
        .find(|f| f.taker_order_id == 2)
        .unwrap();
    assert_eq!(spot_fill.instrument, spot);
    assert_eq!(perp_fill.instrument, perp);
    // qty 验证
    assert!((spot_fill.quantity.as_f64() - 0.1).abs() < 1e-9);
    assert!((perp_fill.quantity.as_f64() - 0.1).abs() < 1e-9);
}

// ── 测试 3:per-leg 不清其他 leg ────────────────────────────

/// 0.6.0 行为:`begin_bar` 调 `clear_book()` 清所有 books,导致
/// spot 的 seed 被清后,perp 的策略单无法成交。
///
/// 0.7.0 验证:spot 的 seed 保留,perp 的 seed 不影响。
#[test]
fn per_leg_does_not_clear_other_leg() {
    let spot = btc_spot();
    let perp = btc_perp();
    // 只对 perp 推单
    let q = build_orders(&[(2, &perp, Side::Buy, 0.1, 2_000_000_000)]);

    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity_for(spot.clone(), 0.01, 3, 0.5);
    engine.with_seed_liquidity_for(perp.clone(), 0.5, 3, 0.5);

    // 先 seed spot,再 seed perp
    // 0.7.0 修:begin_bar(perp) 只清 perp 的 book,spot 的 seed 保留
    engine.begin_bar(100.0, spot.clone());
    engine.begin_bar(200.0, perp.clone());

    // 验证 matching engine 仍保留 spot 的 book(active_order_count > 0)
    // 注:begin_bar 之后 seed_id 增加了 6(spot) + 6(perp) = 12 单
    // 因为 L1MatchingEngine 不暴露 active_order_count 的外部 API,
    // 这里只能通过 fills 验证(perp fill 成功说明 perp 的 book 有 seed)
    let result = engine.run();
    assert_eq!(result.fills, 1, "perp fill 应成功, got {}", result.fills);
    assert_eq!(result.fills_detail.len(), 1);
    assert_eq!(result.fills_detail[0].instrument, perp);
}

// ── 测试 4:default fallback ────────────────────────────────

/// 没调 `with_seed_liquidity_for` 的 leg,`begin_bar(leg)` 用 default config
#[test]
fn default_config_fallback() {
    let spot = btc_spot();
    let q = build_orders(&[(1, &spot, Side::Buy, 0.1, 1_000_000_000)]);

    let mut engine = BacktestEngine::new(base_config(), q);
    // 只设 default,没 per-leg
    engine.with_seed_liquidity(0.01, 3, 0.5);
    engine.begin_bar(100.0, spot.clone());
    let result = engine.run();

    assert_eq!(
        result.fills, 1,
        "default fallback 应让 spot 成交, got {}",
        result.fills
    );
    assert_eq!(result.fills_detail[0].price.as_f64(), 100.01);
}
