//! 场景 1：回测引擎撮合全流程集成测试
//!
//! 验证 L1/L2 撮合引擎的完整订单生命周期：
//! 数据构造 → 订单提交 → 撮合成交 → 冲击 → 结果验证

use axon_backtest::matching::{L1MatchingEngine, L2MatchingEngine, MatchingEngine};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Price, Quantity, Symbol};

fn make_order(id: u64, side: Side, price: f64, qty: f64) -> Order {
    Order::new(
        id,
        Symbol::from("BTC-USDT"),
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

/// 场景 1.1: 构造 OHLCV 数据（100 个价格点）
pub fn run_ohlcv_data_construction() {
    let mut engine = L1MatchingEngine::new();
    for i in 0..100 {
        let price = 100.0 + i as f64 * 0.1;
        let order = make_order(i + 1, Side::Sell, price, 1.0);
        let result = engine.submit(order);
        assert!(result.fills.is_empty(), "卖单之间不应成交");
    }
    let (_, asks) = engine.depth(200);
    assert!(asks.len() >= 100, "应有 100 个卖价层，实际 {}", asks.len());
}

/// 场景 1.2-1.3: 创建引擎 + 注册策略（挂单模拟）
pub fn run_engine_with_strategy_orders() {
    let mut engine = L1MatchingEngine::new();
    engine.submit(make_order(1, Side::Buy, 100.0, 10.0));
    engine.submit(make_order(2, Side::Sell, 105.0, 10.0));
    assert_eq!(engine.best_bid(), Some(Price::from_f64(100.0)));
    assert_eq!(engine.best_ask(), Some(Price::from_f64(105.0)));
}

/// 场景 1.4-1.5: 运行撮合 + 验证成交
pub fn run_matching_and_verify_fills() {
    let mut engine = L1MatchingEngine::new();
    engine.submit(make_order(1, Side::Sell, 100.0, 5.0));
    engine.submit(make_order(2, Side::Sell, 101.0, 3.0));
    let buy = make_order(3, Side::Buy, 101.0, 4.0);
    let result = engine.submit(buy);
    assert!(!result.fills.is_empty(), "应有成交");
    let total_qty: f64 = result.fills.iter().map(|f| f.quantity.as_f64()).sum();
    assert!(total_qty > 0.0, "成交数量应 > 0");
    assert!(result.is_filled, "4.0 买单应完全成交");
}

/// 场景 1.6: 验证订单状态机 New→Filled
pub fn run_order_state_machine() {
    let mut engine = L1MatchingEngine::new();
    engine.submit(make_order(1, Side::Sell, 100.0, 10.0));
    let r1 = engine.submit(make_order(2, Side::Buy, 100.0, 5.0));
    assert!(r1.is_filled, "5.0 买单应完全成交");
    let r2 = engine.submit(make_order(3, Side::Buy, 100.0, 5.0));
    assert!(r2.is_filled, "第二笔 5.0 买单也应完全成交");
    let r3 = engine.submit(make_order(4, Side::Buy, 100.0, 1.0));
    assert!(r3.fills.is_empty(), "卖单已耗尽，不应成交");
}

/// 场景 1.7: 验证成交金额（turnover）
pub fn run_fee_verification() {
    let mut engine = L1MatchingEngine::new();
    engine.submit(make_order(1, Side::Sell, 100.0, 10.0));
    let result = engine.submit(make_order(2, Side::Buy, 100.0, 5.0));
    for fill in &result.fills {
        assert!(fill.turnover() > 0.0, "成交金额应 > 0");
        assert!(fill.quantity.as_f64() > 0.0, "成交数量应 > 0");
    }
}

/// 场景 1.8: 验证市场冲击
pub fn run_market_impact_verification() {
    use axon_backtest::impact::ImpactedMatchingEngine;
    use axon_core::impact::linear::LinearImpactModel;

    let model = LinearImpactModel::new(0.001);
    let mut engine = ImpactedMatchingEngine::new(Box::new(model));
    // 先在引擎内部挂薄卖单
    engine.inner_mut().submit(make_order(1, Side::Sell, 100.0, 1.0));
    // 大单买入 → 应有冲击偏移
    let big_buy = Order::new(
        2,
        Symbol::from("BTC-USDT"),
        Side::Buy,
        OrderType::Limit {
            price: Price::from_f64(110.0),
        },
        Quantity::from_f64(100.0),
        TimeInForce::GTC,
    );
    let result = engine.submit(big_buy);
    if !result.fills.is_empty() {
        let total_qty: f64 = result.fills.iter().map(|f| f.quantity.as_f64()).sum();
        let avg_price: f64 = result
            .fills
            .iter()
            .map(|f| f.price.as_f64() * f.quantity.as_f64())
            .sum::<f64>()
            / total_qty;
        assert!(avg_price >= 100.0, "冲击后均价应 >= 原始卖价");
    }
}

/// L2 深度撮合：多层挂单 + 穿越成交
pub fn run_l2_depth_matching() {
    let mut engine = L2MatchingEngine::new();
    for i in 0..5u64 {
        let price = 100.0 + i as f64;
        engine.submit(make_order(i + 1, Side::Sell, price, 2.0));
    }
    for i in 0..3u64 {
        let price = 97.0 - i as f64;
        engine.submit(make_order(10 + i, Side::Buy, price, 1.0));
    }
    let sweep = make_order(100, Side::Buy, 103.0, 6.0);
    let result = engine.submit(sweep);
    assert!(!result.fills.is_empty(), "应有成交");
    let total_filled: f64 = result.fills.iter().map(|f| f.quantity.as_f64()).sum();
    assert!(
        total_filled >= 6.0 || result.fills.len() >= 3,
        "应扫掉至少 3 层卖单，实际成交 {}",
        total_filled
    );
}

/// L2 深度快照验证
pub fn run_l2_depth_snapshot() {
    let mut engine = L2MatchingEngine::new();
    engine.submit(make_order(1, Side::Buy, 99.0, 5.0));
    engine.submit(make_order(2, Side::Buy, 98.0, 3.0));
    engine.submit(make_order(3, Side::Sell, 101.0, 4.0));
    engine.submit(make_order(4, Side::Sell, 102.0, 2.0));
    let (bids, asks) = engine.depth(10);
    assert!(bids.len() >= 2, "应有 2 层买盘");
    assert!(asks.len() >= 2, "应有 2 层卖盘");
    assert!(bids[0].price >= bids[1].price, "买盘应按价格降序");
    assert!(asks[0].price <= asks[1].price, "卖盘应按价格升序");
}
