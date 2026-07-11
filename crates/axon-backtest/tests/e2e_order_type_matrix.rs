//! 端到端测试:订单类型矩阵(P0-2)
//!
//! ## 测试目标
//!
//! `backtest_e2e_correctness.rs` 覆盖了 SMA → strategy → 撮合的端到端正确性,但只用了
//! 市价单 + 限价单。`axon-core::order::TimeInForce` 还支持 IOC / FOK / GFD,本测试
//! 把每种类型在 E2E 场景中过一遍,验证:
//!
//! 1. 不同 TIF 下的成交/部分成交/挂簿语义正确
//! 2. `orders_accepted` / `fills` / `total_fees` 计数符合手算预期
//! 3. 订单状态机(`is_filled` / 挂簿)与单测一致(`matching/engine.rs` 已有 L1 单测)
//!
//! ## 覆盖矩阵
//!
//! | # | 测试名                              | 订单类型   | 期望 fills | 期望挂簿 |
//! |---|-------------------------------------|-----------|-----------|---------|
//! | 1 | `market_order_fills_against_ask`    | Market    | 1         | 0       |
//! | 2 | `limit_order_buy_fills_when_touched`| Limit(buy)| 1         | 0       |
//! | 3 | `limit_order_buy_no_fill_above`     | Limit(buy)| 0         | 1       |
//! | 4 | `ioc_order_partial_fill_cancels`    | IOC(buy)  | 1 (部分)  | 0       |
//! | 5 | `fok_order_fails_when_depth_insuf`  | FOK(buy)  | 0         | 0       |
//! | 6 | `fok_order_fills_when_depth_suf`    | FOK(buy)  | 1         | 0       |
//! | 7 | `gfd_limit_pending_within_day`      | GFD(buy)  | 0         | 1       |
//!
//! ## 设计要点
//!
//! - **不依赖 SMA 策略**:每个测试只构造最小事件流(sell + buy),手算 expected
//! - **统一 helper**:`make_*_order` 工厂,保证 `TimeInForce` 与订单类型正确搭配
//! - **L1 撮合**:沿用现有 `L1MatchingEngine`(无 impact,无 seed)
//!
//! 运行:`cargo test -p axon-backtest --test e2e_order_type_matrix`

use axon_backtest::engine::{BacktestEngine, BacktestEngineConfig, FeeConfig};
use axon_backtest::matching::L1MatchingEngine;
use axon_core::event::{EventBuilder, OrderAction};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::queue::EventQueue;
use axon_core::scheduler::SimulatedClock;
use axon_core::time::Timestamp;
use axon_core::types::{Price, Quantity, Symbol};

// ── 共享 helper ──────────────────────────────────────────────────────

fn sym() -> Symbol {
    Symbol::from("BTC-USDT")
}

/// 构造限价单 helper
fn make_limit_order(id: u64, side: Side, price: f64, qty: f64, tif: TimeInForce) -> Order {
    Order::new(
        id,
        sym(),
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        tif,
    )
}

/// 构造市价单 helper(撮合器对 Market 要求 IOC,见 `L1MatchingEngine::submit` 注释)
fn make_market_order(id: u64, side: Side, qty: f64) -> Order {
    Order::new(
        id,
        sym(),
        side,
        OrderType::Market,
        Quantity::from_f64(qty),
        TimeInForce::IOC,
    )
}

/// 默认回测配置:无 impact,无 seed_liquidity,无 force_liquidate
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

/// 跑 1 次最小 E2E:卖限价 + 买订单,返回 RunResult
fn run_two_orders(sell: Option<Order>, buy: Order) -> axon_backtest::engine::RunResult {
    let mut q = EventQueue::new();
    let mut b = EventBuilder::new(0);
    if let Some(s) = sell {
        let id = s.id;
        q.push(b.order(Timestamp::from_nanos(1_000), id, OrderAction::Submitted(s)));
    }
    let id = buy.id;
    q.push(b.order(
        Timestamp::from_nanos(2_000),
        id,
        OrderAction::Submitted(buy),
    ));
    let mut engine = BacktestEngine::new(default_config(), q);
    engine.run()
}

// ── 测试 1:市价单吃对手卖单 ─────────────────────────────────────────

/// Market buy 吃 limit sell,期望 1 笔 fill,fee = notional * 0.001
#[test]
fn market_order_fills_against_ask() {
    let sell = make_limit_order(1, Side::Sell, 100.0, 0.1, TimeInForce::GTC);
    let buy = make_market_order(2, Side::Buy, 0.1);

    let result = run_two_orders(Some(sell), buy);

    assert_eq!(result.fills, 1, "应成交 1 笔");
    assert_eq!(result.orders_accepted, 2, "sell + buy 都被接受");
    assert_eq!(result.orders_rejected, 0);

    // 手算:fill @ 100, qty 0.1, notional = 10, fee = 0.01
    let expected_fee = 100.0 * 0.1 * 0.001;
    assert!(
        (result.total_fees - expected_fee).abs() < 1e-9,
        "expected total_fees={}, got {}",
        expected_fee,
        result.total_fees
    );
}

// ── 测试 2:限价单(buy)价格等于最优卖价时成交 ──────────────────────

/// Limit buy @ 99 命中 sell @ 99,期望 1 笔 fill
#[test]
fn limit_order_buy_fills_when_touched() {
    let sell = make_limit_order(1, Side::Sell, 99.0, 0.1, TimeInForce::GTC);
    let buy = make_limit_order(2, Side::Buy, 99.0, 0.1, TimeInForce::GTC);

    let result = run_two_orders(Some(sell), buy);

    assert_eq!(result.fills, 1);
    assert_eq!(result.orders_accepted, 2);
}

// ── 测试 3:限价单(buy)价格低于最优卖价时挂簿 ──────────────────────

/// Limit buy @ 99 不命中 sell @ 100,期望 0 fill,buy 挂簿 accepted
#[test]
fn limit_order_buy_no_fill_above() {
    let sell = make_limit_order(1, Side::Sell, 100.0, 0.1, TimeInForce::GTC);
    let buy = make_limit_order(2, Side::Buy, 99.0, 0.1, TimeInForce::GTC);

    let result = run_two_orders(Some(sell), buy);

    assert_eq!(result.fills, 0, "无成交");
    // sell 挂簿 accepted(无对手方) + buy 挂簿 accepted
    assert_eq!(result.orders_accepted, 2, "两单都挂簿 accepted");
    assert_eq!(result.orders_rejected, 0);
}

// ── 测试 4:IOC 部分成交后剩余取消 ──────────────────────────────────

/// sell 0.05 @ 100,buy IOC 0.1 @ 100:吃 0.05,剩余 0.05 被取消(不挂簿)
#[test]
fn ioc_order_partial_fill_cancels() {
    let sell = make_limit_order(1, Side::Sell, 100.0, 0.05, TimeInForce::GTC);
    let buy = make_limit_order(2, Side::Buy, 100.0, 0.1, TimeInForce::IOC);

    let result = run_two_orders(Some(sell), buy);

    assert_eq!(result.fills, 1, "部分成交 1 笔");
    // sell 挂簿 accepted + buy IOC 接受(部分成交) = 2 accepted, 0 rejected
    assert_eq!(result.orders_accepted, 2);
    // fill qty = 0.05
    // fee = 100 * 0.05 * 0.001 = 0.005
    let expected_fee = 100.0 * 0.05 * 0.001;
    assert!(
        (result.total_fees - expected_fee).abs() < 1e-9,
        "expected total_fees={}, got {}",
        expected_fee,
        result.total_fees
    );
}

// ── 测试 5:FOK 深度不足时整单拒收 ──────────────────────────────────

/// sell 0.05 @ 100,buy FOK 0.1 @ 100:FOK 预检发现深度不足,整单取消
#[test]
fn fok_order_fails_when_depth_insufficient() {
    let sell = make_limit_order(1, Side::Sell, 100.0, 0.05, TimeInForce::GTC);
    let buy = make_limit_order(2, Side::Buy, 100.0, 0.1, TimeInForce::FOK);

    let result = run_two_orders(Some(sell), buy);

    // FOK 预检失败 → 整单被取消(无 fill,无挂簿)
    // 引擎统计:orders_accepted = 1 (sell 挂簿), buy FOK 无 fill 无挂簿 → rejected
    assert_eq!(result.fills, 0, "FOK 整单取消,无 fill");
    assert_eq!(result.orders_rejected, 1, "FOK buy 被拒");
    assert_eq!(result.orders_accepted, 1, "sell 挂簿 accepted");
}

// ── 测试 6:FOK 深度足够时全部成交 ──────────────────────────────────

/// sell 0.5 @ 100,buy FOK 0.1 @ 100:深度足够,全部成交
#[test]
fn fok_order_fills_when_depth_sufficient() {
    let sell = make_limit_order(1, Side::Sell, 100.0, 0.5, TimeInForce::GTC);
    let buy = make_limit_order(2, Side::Buy, 100.0, 0.1, TimeInForce::FOK);

    let result = run_two_orders(Some(sell), buy);

    assert_eq!(result.fills, 1);
    assert_eq!(result.orders_accepted, 2, "sell 挂簿 + FOK 全部成交");
    let expected_fee = 100.0 * 0.1 * 0.001;
    assert!(
        (result.total_fees - expected_fee).abs() < 1e-9,
        "expected total_fees={}, got {}",
        expected_fee,
        result.total_fees
    );
}

// ── 测试 7:GFD(Good For Day)挂簿但无对手方 ─────────────────────────

/// GFD 与 GTC 行为一致:挂簿等待成交。本测试验证 GFD 类型存在且能挂簿
#[test]
fn gfd_limit_pending_within_day() {
    // sell 价高于 buy 价,GFD 挂簿不成交
    let sell = make_limit_order(1, Side::Sell, 100.0, 0.1, TimeInForce::GTC);
    let buy = make_limit_order(2, Side::Buy, 99.0, 0.1, TimeInForce::GFD);

    let result = run_two_orders(Some(sell), buy);

    assert_eq!(result.fills, 0, "价格不交叉 → 无成交");
    assert_eq!(result.orders_accepted, 2, "sell + GFD buy 都挂簿");
    assert_eq!(result.orders_rejected, 0);
}
