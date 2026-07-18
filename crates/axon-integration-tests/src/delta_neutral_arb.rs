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
//! 6. **Funding 结算(Phase C)**:`push_funding` 累加 cash + `total_funding_pnl`,
//!    spot instrument 收到被忽略,`compute_nav` 用 mark 重估未实现 PnL
//! 7. **自动 rebalance(Phase D)**:`set_target_position` + `rebalance_to_target` 把
//!    仓位推到目标,多 leg 同步 rebalance 形成 delta-neutral

// 测试 helper 仅在 `#[test]` 函数中调用,lib build 不报 dead_code
#![allow(dead_code)]

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_backtest::matching::MatchingEngine;
use axon_core::event::{EventBuilder, FundingEvent, MarkEvent, OrderAction};
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
fn push_mark(builder: &mut EventBuilder, queue: &mut EventQueue, mark: MarkEvent) {
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
        let mark1 = MarkEvent {
            instrument: btc_spot(),
            mark_price: Price::from_f64(50_000.0),
            timestamp: Timestamp::from_nanos(1_000_000),
        };
        let mark2 = MarkEvent {
            instrument: btc_spot(),
            mark_price: Price::from_f64(50_500.0),
            timestamp: Timestamp::from_nanos(2_000_000),
        };
        let mark3 = MarkEvent {
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

// ═══════════════════════════════════════════════════════════════════════════
// 场景 6:Funding 结算(Phase C)端到端
// ═══════════════════════════════════════════════════════════════════════════

/// 推 funding 事件到队列的 helper
fn push_funding(builder: &mut EventBuilder, queue: &mut EventQueue, funding: FundingEvent) {
    let event = builder.funding(funding);
    queue.push(event);
}

/// 端到端 funding 结算(0.5.0 Phase C):
/// 1) delta-neutral 入场(spot long 0.1 + perp short 0.1)
/// 2) 推 1 笔 funding 0.0001 @ 50_000:
///    perp short -0.1 × 0.0001 × 50000 × (-1) = +0.5(short 收)
/// 3) 推 spot funding(被忽略)→ total_funding_pnl 不变
/// 4) RunResult.final_nav 反映 cash 增加(0.5 funding + 0 PnL from fill)
#[test]
fn funding_settle_end_to_end_delta_neutral() {
    let mut engine = make_backtest_engine(100_000.0, |b, q| {
        // 入场:spot long 0.1 + perp short 0.1
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
        // 推 perp funding(perp short 收 funding)
        push_funding(
            b,
            q,
            FundingEvent::new(
                btc_perp(),
                0.0001,
                Price::from_f64(50_000.0),
                Timestamp::from_nanos(5_000),
            ),
        );
        // 推 spot funding(被忽略)
        push_funding(
            b,
            q,
            FundingEvent::new(
                btc_spot(),
                0.0001,
                Price::from_f64(50_000.0),
                Timestamp::from_nanos(6_000),
            ),
        );
    });

    let result = engine.run();
    // 入场 2 笔 fill,funding 收到 2 个但只结算 1 个(spot 被忽略)
    assert_eq!(result.fills, 2, "spot + perp 各 1 笔 fill");
    // perp short -0.1 × 0.0001 × 50000 = -0.5(收)cash_delta = +0.5
    assert!(
        (result.total_funding_pnl - 0.5).abs() < 1e-9,
        "perp short funding 应=+0.5,got {}",
        result.total_funding_pnl
    );
    // 终态:spot long +0.1,perp short -0.1(净额 0,delta 中性)
    assert!(
        (result.positions[&btc_spot()] - 0.1).abs() < 1e-9,
        "spot long 应=+0.1"
    );
    assert!(
        (result.positions[&btc_perp()] - (-0.1)).abs() < 1e-9,
        "perp short 应=-0.1"
    );
}

/// 多次 funding 累积(0.5.0 Phase C):3 笔 funding 累计到 total_funding_pnl。
#[test]
fn funding_multiple_settlements_accumulate() {
    let mut engine = make_backtest_engine(100_000.0, |b, q| {
        // 开 perp long 1.0(perp buy 吃 perp sell)
        push_order_submitted(
            b,
            q,
            make_swap_limit(1, "BTC", "USDT", Side::Sell, 50_001.0, 1.0),
            1_000,
        );
        push_order_submitted(
            b,
            q,
            make_swap_limit(2, "BTC", "USDT", Side::Buy, 50_001.0, 1.0),
            2_000,
        );
        // 3 笔 funding 0.0001 @ 50_000:long 1.0 × 0.0001 × 50000 × 3 = 15(付)
        for ts in [3_000, 4_000, 5_000] {
            push_funding(
                b,
                q,
                FundingEvent::new(
                    btc_perp(),
                    0.0001,
                    Price::from_f64(50_000.0),
                    Timestamp::from_nanos(ts),
                ),
            );
        }
    });

    let result = engine.run();
    // 1 笔 fill,3 笔 funding 累加
    assert_eq!(result.fills, 1);
    // long 1.0 × 0.0001 × 50000 × 3 = 15(付)cash_delta = -15
    assert!(
        (result.total_funding_pnl - (-15.0)).abs() < 1e-9,
        "3 笔 funding 累加应=-15,got {}",
        result.total_funding_pnl
    );
}

/// Mark + Funding 联合:mark 价变 → NAV 重估 → funding 用最新 mark 结算
#[test]
fn mark_funding_combined_unrealized_pnl() {
    let mut engine = make_backtest_engine(100_000.0, |b, q| {
        // 开 perp long 1.0
        push_order_submitted(
            b,
            q,
            make_swap_limit(1, "BTC", "USDT", Side::Sell, 50_001.0, 1.0),
            1_000,
        );
        push_order_submitted(
            b,
            q,
            make_swap_limit(2, "BTC", "USDT", Side::Buy, 50_001.0, 1.0),
            2_000,
        );
        // 推 mark 50_100(入场后价格变动,未实现 PnL = (50100 - 50001) * 1 = +99)
        let mark = MarkEvent {
            instrument: btc_perp(),
            mark_price: Price::from_f64(50_100.0),
            timestamp: Timestamp::from_nanos(3_000),
        };
        push_mark(b, q, mark);
        // 推 funding 0.0001 @ 50_100:long 1.0 × 0.0001 × 50100 = 5.01(付)
        push_funding(
            b,
            q,
            FundingEvent::new(
                btc_perp(),
                0.0001,
                Price::from_f64(50_100.0),
                Timestamp::from_nanos(4_000),
            ),
        );
    });

    let result = engine.run();
    // 1 笔 fill
    assert_eq!(result.fills, 1);
    // long funding PnL = -5.01
    assert!(
        (result.total_funding_pnl - (-5.01)).abs() < 1e-6,
        "funding 应=-5.01,got {}",
        result.total_funding_pnl
    );
    // mark 已写入(后到的 mark 50_100 生效)
    assert!(
        (result.marks[&btc_perp()] - 50_100.0).abs() < 1e-9,
        "mark 应=50_100.0,got {}",
        result.marks[&btc_perp()]
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 场景 7:自动 rebalance(Phase D)端到端
// ═══════════════════════════════════════════════════════════════════════════

/// 端到端 rebalance(0.5.0 Phase D):预挂对手盘 → set_target_position → rebalance
/// 触发市价单 → position 推到目标。
///
/// 0.6.0 改(Phase 1):依赖 `begin_bar` 收尾自动 rebalance,不再手写调用。
/// 用户只需 `with_auto_rebalance` + `set_target` + `begin_bar`。
#[test]
fn rebalance_end_to_end_pnl_aware() {
    let mut engine = make_backtest_engine(100_000.0, |b, q| {
        // 预挂 spot sell 0.1(让 rebalance buy 0.1 能撮合)
        push_order_submitted(
            b,
            q,
            make_spot_limit(1, "BTC", "USDT", Side::Sell, 50_001.0, 0.1),
            1_000,
        );
    });

    engine.run();
    // 0.6.0 改:启用自动 rebalance + 设 target + 调 begin_bar 收尾触发
    engine.with_auto_rebalance(1e-6);
    engine.set_target_position(btc_spot(), 0.1);
    engine.begin_bar(50_000.0, btc_spot());
    assert!(
        (engine.get_position(&btc_spot()) - 0.1).abs() < 1e-9,
        "begin_bar 收尾 rebalance 后 spot 应=+0.1"
    );

    let result = engine.run();
    // RunResult.rebalances_triggered 累计到 1
    assert_eq!(
        result.rebalances_triggered, 1,
        "RunResult.rebalances_triggered 应=1"
    );
}

/// 端到端 delta-neutral rebalance:spot long +1 + perp short -1
/// 同步 rebalance → 两 leg 净额 0(delta 中性)
///
/// 0.6.0 改(Phase 1):依赖 `begin_bar` 收尾自动 rebalance。
#[test]
fn rebalance_two_legs_delta_neutral() {
    let mut engine = make_backtest_engine(100_000.0, |b, q| {
        // 预挂 spot sell 1.0 + perp buy 1.0(让 rebalance 双向都能撮合)
        push_order_submitted(
            b,
            q,
            make_spot_limit(1, "BTC", "USDT", Side::Sell, 50_001.0, 1.0),
            1_000,
        );
        push_order_submitted(
            b,
            q,
            make_swap_limit(2, "BTC", "USDT", Side::Buy, 50_001.0, 1.0),
            2_000,
        );
    });

    engine.run();
    // 0.6.0 改:启用 auto_rebalance,设两个 leg target,单次 begin_bar 触发
    // (rebalance 内部遍历所有 legs,与 begin_bar 的 instrument 参数无关)
    engine.with_auto_rebalance(1e-6);
    engine.set_target_position(btc_spot(), 1.0);
    engine.set_target_position(btc_perp(), -1.0);
    engine.begin_bar(50_000.0, btc_spot());
    // 验证 delta-neutral
    let spot_q = engine.get_position(&btc_spot());
    let perp_q = engine.get_position(&btc_perp());
    assert!((spot_q - 1.0).abs() < 1e-9, "spot 应=+1.0,got {spot_q}");
    assert!((perp_q - (-1.0)).abs() < 1e-9, "perp 应=-1.0,got {perp_q}");
    // 净额 0
    assert!(
        (spot_q + perp_q).abs() < 1e-9,
        "两 leg 净额应=0(spot {spot_q} + perp {perp_q})"
    );

    let result = engine.run();
    assert_eq!(result.rebalances_triggered, 2);
}

/// 端到端 rebalance + funding 组合:delta-neutral 入场 + rebalance 触发 +
/// 后续 funding 结算验证整套 0.5.0 Phase C/D 链路。
///
/// 0.6.0 改(Phase 1):入场用 `begin_bar` 收尾自动 rebalance。
#[test]
fn delta_neutral_full_lifecycle_funding_and_rebalance() {
    let mut engine = make_backtest_engine(100_000.0, |b, q| {
        // 1) 预挂 spot ask + perp bid
        push_order_submitted(
            b,
            q,
            make_spot_limit(1, "BTC", "USDT", Side::Sell, 50_001.0, 1.0),
            1_000,
        );
        push_order_submitted(
            b,
            q,
            make_swap_limit(2, "BTC", "USDT", Side::Buy, 50_001.0, 1.0),
            2_000,
        );
    });

    engine.run();

    // 2) 0.6.0 改:auto_rebalance + set_targets + begin_bar 一次性入场
    engine.with_auto_rebalance(1e-6);
    engine.set_target_position(btc_spot(), 1.0);
    engine.set_target_position(btc_perp(), -1.0);
    engine.begin_bar(50_000.0, btc_spot());
    assert!((engine.get_position(&btc_spot()) - 1.0).abs() < 1e-9);
    assert!((engine.get_position(&btc_perp()) - (-1.0)).abs() < 1e-9);

    // 3) 推 funding(perp short 收 funding)
    engine.push_funding(btc_perp(), 0.0001, 50_000.0, Timestamp::from_nanos(3_000));

    let result = engine.run();
    // 关键断言 1:2 笔 rebalance fill
    assert_eq!(result.rebalances_triggered, 2);
    // 关键断言 2:perp short funding PnL = -(-1.0) × 0.0001 × 50000 = +5.0
    assert!(
        (result.total_funding_pnl - 5.0).abs() < 1e-9,
        "perp short funding 应=+5.0,got {}",
        result.total_funding_pnl
    );
    // 关键断言 3:delta-neutral 仍保持(spot long 1.0 + perp short 1.0 = 0)
    let spot_q = result.positions[&btc_spot()];
    let perp_q = result.positions[&btc_perp()];
    assert!(
        (spot_q + perp_q).abs() < 1e-9,
        "delta 中性仍保持(spot {spot_q} + perp {perp_q} = 0)"
    );
}
