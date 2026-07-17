//! Delta-Neutral Arbitrage 集成测试(0.5.0 新增)
//!
//! 验证 spot + swap 两腿(perp)在 `BacktestEngine` 中能独立路由、独立持仓、
//! 互不干扰,这是 0.5.0 引入 `Instrument` 抽象后的核心回归测试。
//!
//! ## 场景覆盖
//!
//! 1. **两腿独立撮合**:spot 与 swap 各自的撮合引擎只对同 instrument 订单响应
//! 2. **两腿独立持仓**:spot long 与 perp short 同时存在,positions dict 两个 key 互不冲突
//! 3. **目标仓位腿 API**:`set_target_position` / `get_target_position` 跨 leg 独立
//! 4. **mark 价格 leg 隔离**:`push_mark` 写入 per-instrument mark cache,后到覆盖
//! 5. **delta-neutral 入场**:`funding > 0 → spot long + perp short` 完整链路
//!
//! ## 设计
//!
//! 这些测试**不**引入 funding 结算(本批 0.5.0 范围不含),只验证腿隔离 /
//! 目标位 / mark cache 等结构层正确性。完整 funding 结算逻辑待 0.6.0 引入。

// 测试 helper 仅在 `#[test]` 函数中调用,lib build 不报 dead_code
#![allow(dead_code)]

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_backtest::matching::MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{
    Instrument, Price, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol,
};

/// 构造 spot limit 订单(测试 helper)
fn make_spot_limit(id: u64, base: &str, quote: &str, side: Side, price: f64, qty: f64) -> Order {
    Order::spot(
        id,
        base,
        quote,
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

/// 构造 swap limit 订单(测试 helper)
fn make_swap_limit(id: u64, base: &str, quote: &str, side: Side, price: f64, qty: f64) -> Order {
    Order::swap(
        id,
        base,
        quote,
        SwapSettle::UsdMargin,
        1.0, // contract_size
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

/// 推一条 `OrderAction::Submitted` 事件到队列(测试 helper)
///
/// `EventBuilder` 需要在外部构造并通过 `&mut` 传入,因为 `order` 方法
/// 内部递增 `seq` 计数器。`ts_ns` 决定事件在 `EventQueue` 中的排序位置
/// (底层为优先级队列,按 `timestamp` 升序出队)。
fn push_order_submitted(
    builder: &mut EventBuilder,
    queue: &mut EventQueue,
    order: Order,
    ts_ns: i64,
) {
    let order_id = order.id;
    let event = builder.order(
        Timestamp::from_nanos(ts_ns),
        order_id,
        OrderAction::Submitted(order),
    );
    queue.push(event);
}

/// 推一条 `Mark` 事件到队列(测试 helper)
fn push_mark(
    builder: &mut EventBuilder,
    queue: &mut EventQueue,
    mark: axon_core::event::MarkEvent,
) {
    let event = builder.mark(mark);
    queue.push(event);
}

/// 构造带 `L1MatchingEngine` 的 `BacktestEngine` + 回调填充事件
///
/// 注意:`BacktestEngine::new(config, queue)` 转移 `EventQueue` 所有权,
/// 所以这里返回 `(engine, queue_ref)` —— `queue_ref` 是 `&mut` 借用,
/// 供测试在创建引擎后**之前**(用 `seed_events` 闭包)填充事件用。
///
/// 由于所有权已转移,测试中只能在创建引擎前预先填充;为此 helper 接受
/// `seed_events: impl FnOnce(&mut EventQueue)` 在转移前 push 事件。
fn make_backtest_engine<F: FnOnce(&mut EventBuilder, &mut EventQueue)>(
    initial_cash: f64,
    seed_events: F,
) -> BacktestEngine {
    let clock = SimulatedClock::new(Timestamp::from_nanos(0));
    let matching: Box<dyn MatchingEngine> = Box::new(L1MatchingEngine::new());
    let config = BacktestEngineConfig {
        clock,
        matching_engine: matching,
        impact_model: None,
        initial_cash,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    };
    let mut queue = EventQueue::new();
    let mut builder = EventBuilder::new(0);
    seed_events(&mut builder, &mut queue);
    BacktestEngine::new(config, queue)
}

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

// ═══════════════════════════════════════════════════════════════════════════
// 场景 1:两腿独立撮合
// ═══════════════════════════════════════════════════════════════════════════

/// Spot sell @ 50_001 + spot buy @ 50_001 → 1 fill(仅 taker 端 buy 被记为开仓)。
///
/// BacktestEngine 现有语义:**只有 taker 端的 fill 推进 position_states**;
/// maker 端(被吃单)不产生 fill 事件,所以其 position 不会变化。
/// 这是"taker 是策略方、maker 是对手盘"的简化视图。
/// 本测试验证:
/// - 两笔订单都被 accepted
/// - 1 笔 fill 发生
/// - 撮合后 spot book 中存在 1 个 taker(long 0.1)位置
#[test]
fn two_legs_spot_match_only_spot_fills() {
    let mut engine = make_backtest_engine(100_000.0, |b, q| {
        // spot 卖 0.1 @ 50_001(挂单,无 fill)
        push_order_submitted(
            b,
            q,
            make_spot_limit(1, "BTC", "USDT", Side::Sell, 50_001.0, 0.1),
            1_000,
        );
        // spot 买 0.1 @ 50_001(taker,吃 sell)
        push_order_submitted(
            b,
            q,
            make_spot_limit(2, "BTC", "USDT", Side::Buy, 50_001.0, 0.1),
            2_000,
        );
        // perp 卖 0.5 @ 50_001(挂单,无 fill)
        push_order_submitted(
            b,
            q,
            make_swap_limit(3, "BTC", "USDT", Side::Sell, 50_001.0, 0.5),
            3_000,
        );
    });

    let result = engine.run();

    // 3 笔订单全 accepted,1 笔 fill(只有 taker buy 0.1 产生 fill)
    assert_eq!(result.orders_accepted, 3, "所有订单应被撮合引擎接收");
    assert_eq!(result.fills, 1, "仅 buy taker 产生 fill");
    // spot taker 端 position = +0.1(buy)
    assert!(
        (engine.get_position(&btc_spot()) - 0.1).abs() < 1e-9,
        "spot taker buy 端仓位应为 +0.1,实际 {}",
        engine.get_position(&btc_spot())
    );
    // perp 无 fill,position 仍为 0(挂单 0.5 不计入仓)
    let perp_pos = engine.get_position(&btc_perp());
    assert!(perp_pos.abs() < 1e-9, "perp 仓位应仍为 0,实际 {perp_pos}");
}

// ═══════════════════════════════════════════════════════════════════════════
// 场景 2:两腿同时挂单,positions dict 两个 instrument 各自独立(都为 0)
// ═══════════════════════════════════════════════════════════════════════════

/// Spot long 0.5 + perp short 0.5:同时 push 两笔订单 → 各自无对手盘 → 都挂单,
/// 各自仓位为 0(挂单不计入仓位)。这是"未使用 with_seed_liquidity"的退化情形。
#[test]
fn two_legs_orders_route_to_independent_books() {
    let mut engine = make_backtest_engine(100_000.0, |b, q| {
        // spot buy 0.5(无对手盘 → 挂单)
        push_order_submitted(
            b,
            q,
            make_spot_limit(1, "BTC", "USDT", Side::Buy, 50_000.0, 0.5),
            1_000,
        );
        // perp sell 0.5(无对手盘 → 挂单)
        push_order_submitted(
            b,
            q,
            make_swap_limit(2, "BTC", "USDT", Side::Sell, 50_000.0, 0.5),
            2_000,
        );
    });

    let result = engine.run();
    // 2 笔 accepted,0 笔 fill(都挂单)
    assert_eq!(result.orders_accepted, 2);
    assert_eq!(result.fills, 0);
    // 两 leg 仓位都为 0(挂单不计入仓,position_states 为空)
    assert!(
        result
            .positions
            .get(&btc_spot())
            .copied()
            .unwrap_or(0.0)
            .abs()
            < 1e-9
    );
    assert!(
        result
            .positions
            .get(&btc_perp())
            .copied()
            .unwrap_or(0.0)
            .abs()
            < 1e-9
    );
    // 由于仓位都 0,positions 字典可能为空(默认过滤 abs > 1e-9)
    // → 验证方法:get_position() 返回 0.0
    assert!(engine.get_position(&btc_spot()).abs() < 1e-9);
    assert!(engine.get_position(&btc_perp()).abs() < 1e-9);
}

// ═══════════════════════════════════════════════════════════════════════════
// 场景 3:目标仓位 leg API
// ═══════════════════════════════════════════════════════════════════════════

/// `set_target_position` 跨 leg 独立,重复设置覆盖前值。
#[test]
fn leg_target_position_independent_per_instrument() {
    let mut engine = make_backtest_engine(100_000.0, |_b, _q| {});

    // 初始:两 leg 都没设过目标
    assert!(engine.get_target_position(&btc_spot()).is_none());
    assert!(engine.get_target_position(&btc_perp()).is_none());

    // 设置 spot long +1,perp short -1(delta-neutral,吃 funding > 0)
    engine.set_target_position(btc_spot(), 1.0);
    engine.set_target_position(btc_perp(), -1.0);

    // 读回
    assert_eq!(engine.get_target_position(&btc_spot()), Some(1.0));
    assert_eq!(engine.get_target_position(&btc_perp()), Some(-1.0));

    // 重复设置 spot 为 2.5,覆盖前值
    engine.set_target_position(btc_spot(), 2.5);
    assert_eq!(engine.get_target_position(&btc_spot()), Some(2.5));
    // perp 不变
    assert_eq!(engine.get_target_position(&btc_perp()), Some(-1.0));
}

// ═══════════════════════════════════════════════════════════════════════════
// 场景 4:Mark 价格 leg 隔离(后到覆盖)
// ═══════════════════════════════════════════════════════════════════════════

/// `push_mark` 推入两 leg 的 mark 价,RunResult.marks 各自独立,后到覆盖先到。
#[test]
fn leg_marks_independent_and_last_wins() {
    let mut engine = make_backtest_engine(100_000.0, |b, q| {
        // spot 推两次(50_000 / 50_500),perp 推一次(50_100)
        let mark1 = axon_core::event::MarkEvent {
            instrument: btc_spot(),
            mark_price: Price::from_f64(50_000.0),
            timestamp: Timestamp::from_nanos(1_000_000),
        };
        let mark2 = axon_core::event::MarkEvent {
            instrument: btc_spot(),
            mark_price: Price::from_f64(50_500.0),
            timestamp: Timestamp::from_nanos(2_000_000),
        };
        let mark3 = axon_core::event::MarkEvent {
            instrument: btc_perp(),
            mark_price: Price::from_f64(50_100.0),
            timestamp: Timestamp::from_nanos(1_500_000),
        };
        push_mark(b, q, mark1);
        push_mark(b, q, mark2);
        push_mark(b, q, mark3);
    });

    let result = engine.run();
    // spot mark 应为 50_500.0(后到覆盖),perp 为 50_100.0
    assert!(
        (result.marks[&btc_spot()] - 50_500.0).abs() < 1e-9,
        "spot mark 应覆盖为 50_500.0,实际 {}",
        result.marks[&btc_spot()]
    );
    assert!(
        (result.marks[&btc_perp()] - 50_100.0).abs() < 1e-9,
        "perp mark 应为 50_100.0,实际 {}",
        result.marks[&btc_perp()]
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 场景 5:Delta-Neutral 入场(spot long + perp short,无 funding 结算,只验证结构)
//
// 完整 funding 结算(funding rate > 0 → spot long + perp short 吃 funding)留
// 给 0.6.0 实现。本测试只验证:两腿订单能被同一撮合引擎分别路由,
// 且 positions dict 两个 instrument 独立累计,无 cross-contamination。
// ═══════════════════════════════════════════════════════════════════════════

/// 模拟 funding > 0 的入场:spot 做多(吃 spot ask)+ perp 做空(吃 perp bid),
/// 同步下两腿,验证两腿 positions dict 独立累计且方向相反(delta 中性)。
///
/// Maker 端(对手盘)是先入的 sell spot + buy perp,策略 taker 后入:
/// - spot buy 0.1 @ 50_001 → 吃 spot sell → spot taker +0.1(long)
/// - perp sell 0.1 @ 50_001 → 吃 perp buy → perp taker -0.1(short)
///
/// 简化的撮合视角:本测试只验证 Instrument 路由层隔离;
/// 完整的 funding 结算(perp 端每期收 funding)留待 0.6.0 实现。
#[test]
fn delta_neutral_entry_orders_isolated() {
    let mut engine = make_backtest_engine(100_000.0, |b, q| {
        // 预置对手盘(maker 端):
        //   - spot ask @ 50_001
        //   - perp bid @ 50_001
        push_order_submitted(
            b,
            q,
            make_spot_limit(1, "BTC", "USDT", Side::Sell, 50_001.0, 0.1),
            1_000,
        );
        push_order_submitted(
            b,
            q,
            make_swap_limit(2, "BTC", "USDT", Side::Buy, 50_001.0, 0.1),
            2_000,
        );

        // 策略 taker 端(delta 中性入场):
        //   - spot long  → buy 吃 spot ask
        //   - perp short → sell 吃 perp bid
        push_order_submitted(
            b,
            q,
            make_spot_limit(3, "BTC", "USDT", Side::Buy, 50_001.0, 0.1),
            3_000,
        );
        push_order_submitted(
            b,
            q,
            make_swap_limit(4, "BTC", "USDT", Side::Sell, 50_001.0, 0.1),
            4_000,
        );
    });

    let result = engine.run();
    // 4 笔 accepted,2 笔 fill(各 leg 一次)
    assert_eq!(result.orders_accepted, 4);
    assert_eq!(result.fills, 2);
    // 关键断言:两 leg 仓位方向相反、数值相等(delta 中性)
    let spot_qty = result.positions[&btc_spot()];
    let perp_qty = result.positions[&btc_perp()];
    assert!(
        (spot_qty - 0.1).abs() < 1e-9,
        "spot long 应为 +0.1,实际 {spot_qty}"
    );
    assert!(
        (perp_qty - (-0.1)).abs() < 1e-9,
        "perp short 应为 -0.1,实际 {perp_qty}"
    );
    // 净额为零(delta 中性)
    assert!(
        (spot_qty + perp_qty).abs() < 1e-9,
        "两 leg 净额应为 0(spot {spot_qty} + perp {perp_qty})"
    );
}
