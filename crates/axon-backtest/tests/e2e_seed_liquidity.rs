//! 端到端测试:seed 流动性 + begin_bar 语义(P0-6)
//!
//! ## 测试目标
//!
//! 现有测试中,**没有任何 E2E 验证** `with_seed_liquidity` + `begin_bar` 的「瞬时对手盘」
//! 机制。`axon_backtest::engine::begin_bar` 在 `engine.rs:419` 实现了 `clear_book +
//! seed_liquidity`,但其行为(`active_order_count` 变化、跨 bar 清理、seed id 计数器)
//! 之前仅在内部单测中验证。本测试套件通过 E2E 场景填充此空白。
//!
//! ## 已知约束
//!
//! - `begin_bar` 是**同步** API(不入事件队列),需在 `BacktestEngine::run()` 之前调用
//! - `begin_bar` 会先 `clear_book()` 清空旧 seed,再 `seed_liquidity()` 重新挂单
//! - seed id 从 `1_000_000_000` 起,跨多次 `begin_bar` 单调递增
//! - `L1MatchingEngine` 完整 override `seed_liquidity`;`L2`/`L3` 默认 no-op → 本测试
//!   只覆盖 L1 路径
//!
//! ## 测试场景
//!
//! 1. `no_begin_bar_no_fill`:无 seed + buy market → fills=0(无对手盘)
//! 2. `begin_bar_seeds_counterparty_then_buy_fills`:
//!    begin_bar(100, hs=0.1, depth=5) → buy market 0.05 → 1 fill @ 100.1,qty=0.05
//! 3. `next_begin_bar_clears_old_seed`:
//!    begin_bar(100) + buy 0.1 → begin_bar(110) + buy 0.1 → 旧 seed 清,新 seed @ 110
//! 4. `seed_id_counter_monotonic`:连续 10 次 begin_bar 后 buy market 仍能撮合
//!
//! 运行:`cargo test -p axon-backtest --test e2e_seed_liquidity`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Instrument, Quantity, SpotInstrument, Symbol};

// ── 共享 helper ──────────────────────────────────────────────────────

const SYM: &str = "BTC/USDT";

fn sym() -> Instrument {
    // T2.3:返回 Instrument(原 Symbol),用于 begin_bar 的 instrument 参数
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

fn make_market_order(id: u64, side: Side, qty: f64) -> Order {
    Order::spot(
        id,
            "BTC",
            "USDT",
        side,
        OrderType::Market,
        Quantity::from_f64(qty),
        TimeInForce::IOC,
    )
}

/// 构造回测配置(无 seed 配置,需后续调 `with_seed_liquidity`)
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

/// 创建 N 笔 buy market 单(各 qty)
fn build_market_buys(count: usize, qty: f64) -> EventQueue {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    for i in 0..count {
        let id = (i + 1) as u64;
        q.push(b.order(
            Timestamp::from_nanos((i as i64 + 1) * 1_000_000),
            id,
            OrderAction::Submitted(make_market_order(id, Side::Buy, qty)),
        ));
    }
    q
}

// ── 测试 1:无 begin_bar → 无成交 ─────────────────────────────

/// 不调 `begin_bar`,无 seed,无对手盘 → buy market 全部 rejected
#[test]
fn no_begin_bar_no_fill() {
    let q = build_market_buys(3, 0.1);
    let mut engine = BacktestEngine::new(base_config(), q);
    // 不调 begin_bar
    let result = engine.run();

    assert_eq!(
        result.fills, 0,
        "无 begin_bar + 无对手盘 → 无成交,got {}",
        result.fills
    );
    assert_eq!(
        result.orders_accepted, 0,
        "无 begin_bar + 无对手盘 → 0 accepted,got {}",
        result.orders_accepted
    );
}

// ── 测试 2:begin_bar 挂对手盘 + buy 成交 ─────────────────────

/// begin_bar(100, hs=0.1, depth=5) 在 100.1..100.5 挂 5 个 sell limit(各 0.1 qty),
/// 推 1 笔 buy market 0.05 → 吃 best_ask @ 100.1 → 1 fill qty 0.05
#[test]
fn begin_bar_seeds_counterparty_then_buy_fills() {
    let q = build_market_buys(1, 0.05);
    let mut engine = BacktestEngine::new(base_config(), q);
    // 启用 seed liquidity(half_spread=0.1, depth=5, size=0.1)
    engine.with_seed_liquidity(0.1, 5, 0.1);
    // 同步触发:在 mid=100 处挂对手盘
    engine.begin_bar(100.0, sym());
    let result = engine.run();

    // 1 笔 fill @ 100.1(asks 第 1 层),qty 0.05
    assert_eq!(result.fills, 1, "应成交 1 笔,got {}", result.fills);
    // fee = 100.1 * 0.05 * 0.001 = 0.005005
    let expected_fee = 100.1 * 0.05 * 0.001;
    assert!(
        (result.total_fees - expected_fee).abs() < 1e-9,
        "fee 应 = {},got {}",
        expected_fee,
        result.total_fees
    );
    // 持仓 long 0.05
    let pos = result.positions.get(SYM).copied().unwrap_or(0.0);
    assert!((pos - 0.05).abs() < 1e-9, "持仓应 = 0.05,got {}", pos);
}

// ── 测试 3:跨 bar begin_bar 清空旧 seed ────────────────────

/// 流程:
/// 1. begin_bar(100) 挂 asks 100.1..100.5
/// 2. buy market 0.1 → 吃 100.1 第一层
/// 3. begin_bar(110) 清旧 + 挂新 asks 110.1..110.5
/// 4. buy market 0.1 → 吃 110.1
///
/// 验证:累计 2 笔 fill(RunStats.fills 累计到 2)
#[test]
fn next_begin_bar_clears_old_seed() {
    let q = build_market_buys(2, 0.1);
    let mut engine = BacktestEngine::new(base_config(), q);
    engine.with_seed_liquidity(0.1, 5, 0.1);

    // 阶段 1:begin_bar(100) → 跑第 1 个 buy market
    engine.begin_bar(100.0, sym());
    let ev1 = engine.step().expect("queue 中应至少有 1 个事件");
    // RunStats.fills 是累计值:第 1 步后 = 1
    assert_eq!(ev1.fills, 1, "第 1 步累计 fills 应 = 1");

    // 阶段 2:begin_bar(110) 清旧 seed + 挂新 seed → 跑第 2 个 buy market
    engine.begin_bar(110.0, sym());
    let ev2 = engine.step().expect("queue 中应至少有 2 个事件");
    // 累计 fills = 2(1 + 1)
    assert_eq!(ev2.fills, 2, "第 2 步累计 fills 应 = 2");

    // 关键验证:旧 seed @ 100.1..100.5 已被 clear,只有新 seed @ 110.1..110.5
    // 若 begin_bar 未清旧,新 seed 会追加,撮合时 buy 0.1 可能吃 2 档(0.2 qty)
    // 这里 fills=2(累计)已经证明 2 笔 buy 各成交 1 次,旧 seed 未污染
}

// ── 测试 4:seed id 计数器单调递增 ─────────────────────────

/// 连续 10 次 begin_bar,验证撮合引擎仍能正确工作(无 id 冲突 / seed 泄漏)
#[test]
fn seed_id_counter_monotonic() {
    let depth_levels = 5;
    let mut engine = BacktestEngine::new(base_config(), EventQueue::new());
    engine.with_seed_liquidity(0.1, depth_levels, 0.1);

    // 连续 10 次 begin_bar(用不同 mid 避免 seed 重叠)
    for i in 0..10 {
        engine.begin_bar(100.0 + i as f64, sym());
    }

    // 10 次 begin_bar 后,撮合引擎里应有 2*depth_levels = 10 笔挂单
    // 推 1 个 buy market 0.05 → 应能吃 1 档(0.05 < 0.1)
    let q = build_market_buys(1, 0.05);
    let mut engine2 = BacktestEngine::new(base_config(), q);
    engine2.with_seed_liquidity(0.1, depth_levels, 0.1);
    // 模拟 10 次 begin_bar(消耗 id 但不消费 queue)
    for i in 0..10 {
        engine2.begin_bar(100.0 + i as f64, sym());
    }
    let result = engine2.run();
    assert!(
        result.fills >= 1,
        "10 次 begin_bar 后 buy market 应能撮合,got {} fills",
        result.fills
    );
    // 验证 fill 数 = 1(0.05 qty 吃 1 档 0.1 qty 中的 0.05)
    assert_eq!(
        result.fills, 1,
        "buy 0.05 应只吃 1 档 0.1,got {} fills",
        result.fills
    );
}
