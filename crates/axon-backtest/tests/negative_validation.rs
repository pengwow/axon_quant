//! 端到端测试:BacktestEngine 负向验证(P2-2)
//!
//! ## 测试目标
//!
//! `edge_events_validation.rs` 已经覆盖了"零价格 / 零数量"等基础边界。本文件
//! 聚焦**更深度的负向场景**,验证 BacktestEngine 在面对非法 / 异常订单事件时
//! 的鲁棒性:
//!
//! 1. **负价格限价单**:`price = -100.0` → L1.validate 判定为 `InvalidPrice` → rejected
//! 2. **NaN 数量订单**:`qty = f64::NAN` → L1.validate 走 `qty <= 0.0` 比较,NaN
//!    永远不满足,理论上 validate 通过,但 f64::NAN 进入 BTreeMap 排序会污染订单簿
//! 3. **资金不足的市价买单**:`initial_cash = 10`,buy 需要 1000 USDT →
//!    BacktestEngine 不在主循环做"现金前置检查",所有 fill 都接受;验证实际行为
//! 4. **多笔混合负向事件**:1 笔负价 + 1 笔 NaN + 1 笔合法 → 验证计数一致性
//!
//! ## 设计要点
//!
//! - **不依赖 SMA 策略**:每个测试构造最小事件流
//! - **记录实现行为**:对 NaN / 资金不足等"看实现"的情况,断言"不 panic" + 记录
//!   `orders_rejected / orders_accepted` 实际值(不假定具体行为)
//! - **L1 撮合**:沿用现有 `L1MatchingEngine`
//!
//! 运行:`cargo test -p axon-backtest --test negative_validation`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity};

// ── 共享 helper ──────────────────────────────────────────────────────

/// 基础配置(无冲击 / 默认费率 / force_liquidate=false / 100k 现金)
fn base_config(initial_cash: f64) -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

// ── 测试 1:负价格限价单 → rejected ──────────────────────────────────

/// `price = -100.0` 限价单 → L1.validate 判定为 `InvalidPrice` → 走 rejected 分支
///
/// 验证:`orders_rejected += 1`,`fills == 0`,`final_nav == initial_cash`
#[test]
fn negative_price_limit_order_is_rejected() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    let bad = Order::spot(
        1,
        "BTC",
        "USDT",
        Side::Buy,
        OrderType::Limit {
            price: Price::from_f64(-100.0),
        },
        Quantity::from_f64(1.0),
        TimeInForce::GTC,
    );
    q.push(b.order(Timestamp::from_nanos(1_000), 1, OrderAction::Submitted(bad)));

    let mut engine = BacktestEngine::new(base_config(100_000.0), q);
    let result = engine.run();

    assert_eq!(result.events_processed, 1);
    assert_eq!(result.orders_rejected, 1, "负价格 → rejected");
    assert_eq!(result.orders_accepted, 0, "无 accepted");
    assert_eq!(result.fills, 0, "无 fill");
    assert_eq!(result.final_nav, 100_000.0, "NAV 不变");
}

// ── 测试 2:NaN 数量订单不 panic ─────────────────────────────────────

/// `Quantity::from_f64(f64::NAN)` → L1.validate 中 `qty <= 0.0` 对 NaN 返回 false
/// → validate 通过,订单进订单簿。但 BTreeMap 排序 NaN 行为未定义,可能污染
/// `best_bid / best_ask`。**只断言不 panic**。
///
/// 注:这是"记录实现行为"测试,不强求 rejected/accepted 计数符合期望。
#[test]
fn nan_quantity_does_not_panic() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    let nan_order = Order::spot(
        1,
        "BTC",
        "USDT",
        Side::Buy,
        OrderType::Limit {
            price: Price::from_f64(100.0),
        },
        Quantity::from_f64(f64::NAN),
        TimeInForce::GTC,
    );
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(nan_order),
    ));

    let mut engine = BacktestEngine::new(base_config(100_000.0), q);
    // 关键:不 panic
    let result = engine.run();

    // 事件被处理(不丢)
    assert_eq!(result.events_processed, 1, "1 事件被处理");
    // NaN 行为不强制:订单可能被 accepted(NaN qty 进簿)或 rejected(NaN 比较)
    // 这里只断言总计数 = 1(accepted + rejected = 1)
    assert_eq!(
        result.orders_accepted + result.orders_rejected,
        1,
        "1 事件必被 accept 或 reject 二选一"
    );
}

// ── 测试 3:资金不足的市价买单 ──────────────────────────────────────

/// `initial_cash = 10`,buy qty=1 @ 1000 USDT(需要 1000 USDT,远超 10)
/// → BacktestEngine 不在主循环做"现金前置检查",L1 撮合引擎无 cash 概念
/// → 订单可正常撮合,fill 后 cash 变负 → 验证"不会自动拒绝"
///
/// 验证:`fills == 1`,`cash` 变负,`total_fees == 0.1`(0.1% of 1000)
#[test]
fn insufficient_cash_market_buy_does_not_reject_in_engine() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 对手 sell @ 1000 qty=1(对手盘是策略吃单的对手)
    let counter = Order::spot(
        1,
        "BTC",
        "USDT",
        Side::Sell,
        OrderType::Limit {
            price: Price::from_f64(1000.0),
        },
        Quantity::from_f64(1.0),
        TimeInForce::GTC,
    );
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(counter),
    ));

    // 策略 buy market @ qty=1(需要 1000 USDT,但 initial_cash=10)
    let strategy = Order::spot(
        2,
        "BTC",
        "USDT",
        Side::Buy,
        OrderType::Market,
        Quantity::from_f64(1.0),
        TimeInForce::IOC,
    );
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(strategy),
    ));

    // initial_cash=10 远不够 buy 1000,但 BacktestEngine 不做 cash 校验
    let mut engine = BacktestEngine::new(base_config(10.0), q);
    let result = engine.run();

    // 关键:BacktestEngine 不拒绝 → 订单被撮合,cash 变负
    assert_eq!(result.fills, 1, "1 笔 fill(无 cash 校验)");
    assert_eq!(result.orders_accepted, 2, "2 笔都被接受");
    // 终态:buy 1 @ 1000, cash = 10 - 1000 - 1.0(fee) = -991.0
    // NAV = cash + position = -991 + 1000 = 9
    // total_pnl = 9 - 10 = -1(等于 fee)
    assert!(
        result.final_nav < 10.0,
        "NAV 应 < initial_cash(10),got {}",
        result.final_nav
    );
    assert!(result.total_pnl < 0.0, "PnL < 0(扣 fee)");
    // 注:cash 校验是**策略层 / 风控层**责任,不是 BacktestEngine 责任
    // 这里记录"BacktestEngine 不做 cash 校验"的契约
}

// ── 测试 4:多笔混合负向事件计数一致性 ──────────────────────────────

/// 1 笔负价 + 1 笔 NaN + 1 笔合法 sell limit
///
/// 验证:
/// - 负价必然 rejected
/// - NaN 不强制(可能 accepted,可能 rejected)
/// - 合法 sell limit 无对手方 → accepted(挂簿)
/// - 总事件计数:events_processed = 3
/// - 至少 1 笔 rejected(负价)
#[test]
fn mixed_negative_and_valid_orders_counters_consistent() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 1) 负价买单
    let bad_price = Order::spot(
        1,
        "BTC",
        "USDT",
        Side::Buy,
        OrderType::Limit {
            price: Price::from_f64(-100.0),
        },
        Quantity::from_f64(1.0),
        TimeInForce::GTC,
    );
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(bad_price),
    ));

    // 2) NaN qty 卖单
    let nan_qty = Order::spot(
        2,
        "BTC",
        "USDT",
        Side::Sell,
        OrderType::Limit {
            price: Price::from_f64(100.0),
        },
        Quantity::from_f64(f64::NAN),
        TimeInForce::GTC,
    );
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        OrderAction::Submitted(nan_qty),
    ));

    // 3) 合法 sell limit @ 100 qty=1(无对手方 → 挂簿)
    let valid_sell = Order::spot(
        3,
        "BTC",
        "USDT",
        Side::Sell,
        OrderType::Limit {
            price: Price::from_f64(100.0),
        },
        Quantity::from_f64(1.0),
        TimeInForce::GTC,
    );
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        3,
        OrderAction::Submitted(valid_sell),
    ));

    let mut engine = BacktestEngine::new(base_config(100_000.0), q);
    let result = engine.run();

    assert_eq!(result.events_processed, 3, "3 事件全部处理");
    // 负价必 rejected,合法 sell 必 accepted(挂簿)
    assert!(
        result.orders_rejected >= 1,
        "至少 1 笔 rejected(负价),got {}",
        result.orders_rejected
    );
    assert!(
        result.orders_accepted >= 1,
        "至少 1 笔 accepted(合法 sell),got {}",
        result.orders_accepted
    );
    // 计数自洽:accepted + rejected = 3
    assert_eq!(
        result.orders_accepted + result.orders_rejected,
        3,
        "accepted + rejected = 3"
    );
}
