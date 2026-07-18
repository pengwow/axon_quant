//! 端到端测试:边界事件验证(W-5)
//!
//! ## 测试目标
//!
//! v1 的 e2e_*.rs 覆盖了「业务路径」(E2E 撮合 + 6 状态机),但**没有专门验证
//! 边界事件**的语义契约。本测试聚焦 4 个最容易被忽略的边界:
//!
//! 1. **空事件队列**:无任何事件 → 全零结果
//! 2. **乱序时间戳**:ts 不按 push 顺序入队,EventQueue 仍按 (ts, seq) 升序处理
//! 3. **零价格限价单**:OrderType::Limit{price: 0} → orders_rejected
//! 4. **零数量限价单**:Quantity::from_f64(0.0) → orders_rejected
//!
//! ## 设计要点
//!
//! - **不依赖 SMA / 撮合对手盘**:每个测试只构造最小事件,断言「计数 + 终态字段」
//! - **L1 撮合**:沿用现有 `L1MatchingEngine`(无 impact / seed)
//! - **手算对账**:每个测试的 `orders_accepted / rejected` 计数可从 L1.validate 推导
//!
//! 运行:`cargo test -p axon-backtest --test edge_events_validation`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{Event, EventBuilder, OrderEvent};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity};

// ── 共享 helper ──────────────────────────────────────────────────────

/// 构造限价单 helper(合法)
fn make_limit_order(id: u64, side: Side, price: f64, qty: f64) -> Order {
    Order::spot(
        id,
        "BTC",
        "USDT",
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

/// 基础配置(无冲击 / 默认费率 / force_liquidate=false)
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

// ── 测试 1:空事件队列 ────────────────────────────────────────────────

/// 空 EventQueue → run() 全零返回,final_nav == initial_cash
///
/// 验证边界:无事件时 engine 不应 panic,所有计数 = 0,NAV 保持初始值
#[test]
fn empty_queue_run_returns_zero_filled() {
    let mut engine = BacktestEngine::new(base_config(100_000.0), EventQueue::new());
    let result = engine.run();

    assert_eq!(result.events_processed, 0, "空队列无事件");
    assert_eq!(result.fills, 0, "无 fill");
    assert_eq!(result.orders_accepted, 0, "无 accepted");
    assert_eq!(result.orders_rejected, 0, "无 rejected");
    assert_eq!(result.orders_cancelled, 0, "无 cancelled");
    assert_eq!(result.orders_modified, 0, "无 modified");
    assert_eq!(result.total_pnl, 0.0, "total_pnl = 0");
    assert_eq!(result.final_nav, 100_000.0, "final_nav = initial_cash");
    assert_eq!(result.max_drawdown, 0.0, "max_drawdown = 0");
    assert_eq!(result.equity_curve.len(), 0, "equity_curve 为空");
    assert_eq!(result.trades.len(), 0, "无 trade");
    assert!(result.positions.is_empty(), "无 position");
}

// ── 测试 2:乱序时间戳事件全部被处理 ────────────────────────────────

/// ts 乱序 push(ts=3k/1k/2k)→ EventQueue 仍按 (ts, seq) 升序处理
///
/// 关键验证:`events_processed == 3`(不丢失),`final_time == max(ts) = 3_000`
///
/// 注:不验证处理顺序(由 EventQueue 实现保证;nav_dd_consistency 已覆盖 seq 同 ts 顺序)
#[test]
fn out_of_order_timestamps_still_drained() {
    let mut q = EventQueue::new();

    // 手动 push 3 个 Cancelled 事件,ts 乱序
    q.push(Event::Order(OrderEvent {
        seq: 0,
        timestamp: Timestamp::from_nanos(3_000),
        order_id: 1,
        action: axon_core::event::OrderAction::Cancelled(1),
    }));
    q.push(Event::Order(OrderEvent {
        seq: 1,
        timestamp: Timestamp::from_nanos(1_000),
        order_id: 2,
        action: axon_core::event::OrderAction::Cancelled(2),
    }));
    q.push(Event::Order(OrderEvent {
        seq: 2,
        timestamp: Timestamp::from_nanos(2_000),
        order_id: 3,
        action: axon_core::event::OrderAction::Cancelled(3),
    }));

    let mut engine = BacktestEngine::new(base_config(100_000.0), q);
    let result = engine.run();

    assert_eq!(result.events_processed, 3, "3 个事件全部处理");
    assert_eq!(result.orders_cancelled, 3, "3 个 cancelled 计数");
    // final_time 应 = max(ts) = 3_000
    assert_eq!(
        result.final_time.nanos, 3_000,
        "final_time 应 = 最后事件 ts = 3_000"
    );
}

// ── 测试 3:零价格限价单 → rejected ─────────────────────────────────

/// price=0 限价单 → L1.validate 失败 → SubmitResult::empty → active_count 不变
/// → handle_submit 走 (true, false) 分支 → orders_rejected += 1
#[test]
fn zero_price_limit_order_is_rejected() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 构造 price=0 的非法限价单
    let bad = Order::spot(
        1,
        "BTC",
        "USDT",
        Side::Buy,
        OrderType::Limit {
            price: Price::from_f64(0.0),
        },
        Quantity::from_f64(1.0),
        TimeInForce::GTC,
    );
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        axon_core::event::OrderAction::Submitted(bad),
    ));

    let mut engine = BacktestEngine::new(base_config(100_000.0), q);
    let result = engine.run();

    assert_eq!(result.events_processed, 1, "1 事件");
    assert_eq!(result.orders_rejected, 1, "零价格 → rejected");
    assert_eq!(result.orders_accepted, 0, "无 accepted");
    assert_eq!(result.fills, 0, "无 fill");
    assert_eq!(result.total_pnl, 0.0, "PnL 不变");
    assert_eq!(result.final_nav, 100_000.0, "NAV 不变");
}

// ── 测试 4:零数量限价单 → rejected ──────────────────────────────────

/// qty=0 限价单 → L1.validate 失败 → 走 rejected 分支
#[test]
fn zero_quantity_limit_order_is_rejected() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 构造 qty=0 的非法限价单
    let bad = Order::spot(
        1,
        "BTC",
        "USDT",
        Side::Buy,
        OrderType::Limit {
            price: Price::from_f64(100.0),
        },
        Quantity::from_f64(0.0),
        TimeInForce::GTC,
    );
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        axon_core::event::OrderAction::Submitted(bad),
    ));

    let mut engine = BacktestEngine::new(base_config(100_000.0), q);
    let result = engine.run();

    assert_eq!(result.events_processed, 1);
    assert_eq!(result.orders_rejected, 1, "零数量 → rejected");
    assert_eq!(result.orders_accepted, 0);
    assert_eq!(result.fills, 0);
    assert_eq!(result.final_nav, 100_000.0);
}

// ── 隐藏 bonus 验证:零价 / 零量 同时出现仍 rejected(幂等) ───────────

/// 同一 bar 同时推送 1 个零价 + 1 个零量 + 1 个合法 sell limit
///
/// 验证:
/// - 零价 / 零量 都 rejected
/// - 合法 sell limit 被挂簿(无对手方)→ accepted
/// - 计数:events_processed=3, orders_accepted=1, orders_rejected=2
#[test]
fn mixed_invalid_and_valid_orders_counters_consistent() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 1) 零价买单
    let bad_price = Order::spot(
        1,
        "BTC",
        "USDT",
        Side::Buy,
        OrderType::Limit {
            price: Price::from_f64(0.0),
        },
        Quantity::from_f64(1.0),
        TimeInForce::GTC,
    );
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        axon_core::event::OrderAction::Submitted(bad_price),
    ));

    // 2) 零量卖单
    let bad_qty = Order::spot(
        2,
        "BTC",
        "USDT",
        Side::Sell,
        OrderType::Limit {
            price: Price::from_f64(100.0),
        },
        Quantity::from_f64(0.0),
        TimeInForce::GTC,
    );
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        2,
        axon_core::event::OrderAction::Submitted(bad_qty),
    ));

    // 3) 合法 sell limit(无对手方 → 挂簿 → accepted)
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        3,
        axon_core::event::OrderAction::Submitted(make_limit_order(3, Side::Sell, 100.0, 1.0)),
    ));

    let mut engine = BacktestEngine::new(base_config(100_000.0), q);
    let result = engine.run();

    assert_eq!(result.events_processed, 3);
    assert_eq!(result.orders_accepted, 1, "合法 sell limit accepted");
    assert_eq!(result.orders_rejected, 2, "零价 + 零量都 rejected");
    assert_eq!(result.fills, 0, "无 fill(挂簿不成交)");
    assert_eq!(result.final_nav, 100_000.0, "NAV 不变");
}
