//! 端到端测试:撤单 / 改单 / 拒单事件计数(P0-4)
//!
//! ## 测试目标
//!
//! 现有 E2E 测试只覆盖了 `OrderAction::Submitted` 路径,没碰 `Cancelled` /
//! `Modified` / `Rejected` 事件。`axon_backtest::engine::handle_order_action`
//! 收到这三种事件时**只累加对应计数器**,**不会**调 `MatchingEngine::cancel` /
//! `MatchingEngine::modify`(已知限制)。
//!
//! 本测试套件验证:
//! 1. Cancelled / Modified / Rejected 事件被正确计数
//! 2. Cancelled 事件**不会**真从订单簿撤单(订单簿残留)
//! 3. Cancelled 事件不会产生额外 fill / fee
//!
//! ## 已知限制(P1 加固项)
//!
//! 当前 `handle_order_action` 中:
//! ```ignore
//! OrderAction::Cancelled(_) => self.stats.orders_cancelled += 1,
//! OrderAction::Modified { .. } => self.stats.orders_modified += 1,
//! OrderAction::Rejected { .. } => self.stats.orders_rejected += 1,
//! ```
//! **未**调 `MatchingEngine::cancel / modify`。这意味着 Cancelled 事件不会真
//! 从撮合器撤单,残留订单仍可被后续 fill 撮合。**这是有意测试的边界**。
//!
//! ## 测试场景
//!
//! 1. `cancelled_event_increments_counter`:3 个 Cancelled 事件 → counter = 3
//! 2. `modified_event_increments_counter`:2 个 Modified 事件 → counter = 2
//! 3. `rejected_event_increments_counter`:1 个 Rejected 事件 → counter = 1
//! 4. `cancel_after_submit_does_not_remove_pending_order`:已知限制,订单簿残留
//!
//! 运行:`cargo test -p axon-backtest --test e2e_cancel_amend`

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

fn default_config() -> BacktestEngineConfig {
    BacktestEngineConfig {
        clock: SimulatedClock::new(Timestamp::from_nanos(0)),
        matching_engine: Box::new(L1MatchingEngine::new()),
        impact_model: None,
        initial_cash: 100_000.0,
        fee_config: FeeConfig::default(),
        force_liquidate: false,
    }
}

// ── 测试 1:Cancelled 事件计数 ────────────────────────────────────────

/// 推 3 个 Cancelled 事件,验证 `result.orders_cancelled == 3`
#[test]
fn cancelled_event_increments_counter() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    for i in 1i64..=3 {
        q.push(b.order(
            Timestamp::from_nanos(i * 1_000i64),
            i as u64,
            OrderAction::Cancelled(i as u64),
        ));
    }

    let mut engine = BacktestEngine::new(default_config(), q);
    let result = engine.run();

    assert_eq!(result.orders_cancelled, 3);
    assert_eq!(result.orders_modified, 0);
    assert_eq!(result.orders_rejected, 0);
    assert_eq!(result.events_processed, 3);
}

// ── 测试 2:Modified 事件计数 ──────────────────────────────────────────

/// 推 2 个 Modified 事件,验证 `result.orders_modified == 2`
///
/// 注:`OrderAction::Modified` 只含 `new_quantity` 字段(无 `new_price`),
/// 验证 API 边界
#[test]
fn modified_event_increments_counter() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    for i in 1i64..=2 {
        q.push(b.order(
            Timestamp::from_nanos(i * 1_000i64),
            i as u64,
            OrderAction::Modified {
                order_id: i as u64,
                new_quantity: Quantity::from_f64(5.0),
            },
        ));
    }

    let mut engine = BacktestEngine::new(default_config(), q);
    let result = engine.run();

    assert_eq!(result.orders_modified, 2);
    assert_eq!(result.orders_cancelled, 0);
    assert_eq!(result.orders_rejected, 0);
    assert_eq!(result.events_processed, 2);
}

// ── 测试 3:Rejected 事件计数 ──────────────────────────────────────────

/// 推 1 个 Rejected 事件(带 reason),验证 `result.orders_rejected == 1`
#[test]
fn rejected_event_increments_counter() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Rejected {
            order_id: 1,
            reason: "risk limit exceeded".into(),
        },
    ));

    let mut engine = BacktestEngine::new(default_config(), q);
    let result = engine.run();

    assert_eq!(result.orders_rejected, 1);
    assert_eq!(result.orders_cancelled, 0);
    assert_eq!(result.orders_modified, 0);
    assert_eq!(result.events_processed, 1);
}

// ── 测试 4:Cancelled 后订单簿残留(已知限制) ─────────────────────────

/// **已知限制**: Cancelled 事件不调 `MatchingEngine::cancel`,订单簿残留。
///
/// 场景:
/// 1. 推 1 个 sell limit @ 100 qty 1.0 → 挂簿,active_order_count = 1
/// 2. 推 1 个 Cancelled 事件(id=1) → counter +1,**但订单簿仍残留**
/// 3. 推 1 个 buy market → 吃残留 sell,1 笔 fill
///
/// 验证:
/// - `orders_cancelled = 1`(事件被计数)
/// - `fills = 1`(残留订单仍可被撮合,这是 P1 加固项)
/// - `positions["BTC/USDT"] = 0.1`(实际成交了)
#[test]
fn cancel_after_submit_does_not_remove_pending_order() {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);

    // 1) sell 挂簿
    q.push(b.order(
        Timestamp::from_nanos(1_000),
        1,
        OrderAction::Submitted(make_limit_order(1, Side::Sell, 100.0, 1.0)),
    ));
    // 2) Cancelled 事件(已知:不真撤单)
    q.push(b.order(Timestamp::from_nanos(2_000), 1, OrderAction::Cancelled(1)));
    // 3) buy market → 吃 sell
    q.push(b.order(
        Timestamp::from_nanos(3_000),
        2,
        OrderAction::Submitted(Order::spot(
            2,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Market,
            Quantity::from_f64(0.1),
            TimeInForce::IOC,
        )),
    ));

    let mut engine = BacktestEngine::new(default_config(), q);
    let result = engine.run();

    // Cancelled 事件被计数
    assert_eq!(result.orders_cancelled, 1, "Cancelled 事件被计数");
    // 卖单未真撤 → 后续 buy market 仍可撮合
    assert_eq!(result.fills, 1, "残留 sell 仍被撮合(P1 加固项:真撤单)");
    // fill 价格 = 100,qty = 0.1,fee = 0.01
    assert!(
        (result.total_fees - (100.0 * 0.1 * 0.001)).abs() < 1e-9,
        "fee 应 = 0.01,got {}",
        result.total_fees
    );
    // 持仓
    assert!(
        (result.positions["BTC/USDT"] - 0.1).abs() < 1e-9,
        "BTC 持仓 0.1,got {}",
        result.positions["BTC/USDT"]
    );
}
