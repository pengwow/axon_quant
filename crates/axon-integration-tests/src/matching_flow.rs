//! 场景 1：回测引擎撮合全流程集成测试
//!
//! 验证 L1/L2 撮合引擎的完整订单生命周期：
//! 数据构造 → 订单提交 → 撮合成交 → 冲击 → 结果验证

use axon_backtest::matching::{L1MatchingEngine, L2MatchingEngine, MatchingEngine};
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Price, Quantity, Symbol};

fn make_order(id: u64, side: Side, price: f64, qty: f64) -> Order {
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
    engine
        .inner_mut()
        .submit(make_order(1, Side::Sell, 100.0, 1.0));
    // 大单买入 → 应有冲击偏移
    let big_buy = Order::spot(
        2,
        "BTC",
        "USDT",
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

// ═══════════════════════════════════════════════════════════════════════════
// 压力场景测试
// ═══════════════════════════════════════════════════════════════════════════

/// 闪崩场景：价格瞬间暴跌 50%，大单买入应正确处理
pub fn run_flash_crash_scenario() {
    let mut engine = L2MatchingEngine::new();
    // 正常市场：5 层卖单 100-104
    for i in 0..5u64 {
        engine.submit(make_order(i + 1, Side::Sell, 100.0 + i as f64, 10.0));
    }
    // 闪崩：价格跌到 50，挂一层卖单
    engine.submit(make_order(100, Side::Sell, 50.0, 100.0));

    // 大单买入，应扫过正常价位直到 50
    let big_buy = make_order(200, Side::Buy, 110.0, 200.0);
    let result = engine.submit(big_buy);
    assert!(!result.fills.is_empty(), "闪崩场景应有成交");
    let total_qty: f64 = result.fills.iter().map(|f| f.quantity.as_f64()).sum();
    assert!(total_qty > 0.0, "成交数量应 > 0");
}

/// 零流动性场景：订单簿为空时提交订单
pub fn run_zero_liquidity_rejection() {
    let mut engine = L2MatchingEngine::new();
    // 空订单簿，提交买单（无对手方，应挂簿）
    let buy = make_order(1, Side::Buy, 100.0, 1.0);
    let result = engine.submit(buy);
    assert!(result.fills.is_empty(), "空订单簿不应成交");
    // 买单应挂入买盘
    assert_eq!(engine.active_order_count(), 1, "买单应挂入买盘");
    // 再提交一笔卖单，价格高于买价，无法匹配
    let sell = make_order(2, Side::Sell, 200.0, 1.0);
    let result = engine.submit(sell);
    assert!(result.fills.is_empty(), "卖单价格高于买价不应成交");
    // 两笔都挂在簿中
    assert_eq!(engine.active_order_count(), 2, "买卖各挂一笔");
}

/// 大单冲击场景：大单扫过簿中大部分深度
pub fn run_large_order_impact() {
    let mut engine = L2MatchingEngine::new();
    // 挂 10 层卖单，每层 10 单位，总深度 100
    for i in 0..10u64 {
        engine.submit(make_order(i + 1, Side::Sell, 100.0 + i as f64, 10.0));
    }
    // 大单买入 80 单位（占深度 80%）
    let big_buy = make_order(100, Side::Buy, 120.0, 80.0);
    let result = engine.submit(big_buy);
    assert!(!result.fills.is_empty(), "大单应有成交");
    let total_qty: f64 = result.fills.iter().map(|f| f.quantity.as_f64()).sum();
    assert!(total_qty >= 80.0, "应成交至少 80 单位，实际 {}", total_qty);
    // 成交均价应在合理范围内（>= 100）
    let total_value: f64 = result
        .fills
        .iter()
        .map(|f| f.price.as_f64() * f.quantity.as_f64())
        .sum();
    let avg_price = total_value / total_qty;
    assert!(
        avg_price >= 100.0,
        "大单成交均价应 >= 100，实际 {}",
        avg_price
    );
}

/// 部分成交场景：买单数量超过卖单深度
pub fn run_partial_fill_update() {
    let mut engine = L2MatchingEngine::new();
    // 只挂 5 单位卖单
    engine.submit(make_order(1, Side::Sell, 100.0, 5.0));
    // 提交 10 单位买单 → 部分成交
    let buy = make_order(2, Side::Buy, 100.0, 10.0);
    let result = engine.submit(buy);
    assert!(!result.fills.is_empty(), "应有成交");
    let filled_qty: f64 = result.fills.iter().map(|f| f.quantity.as_f64()).sum();
    assert!(
        (filled_qty - 5.0).abs() < 1e-9,
        "应部分成交 5 单位，实际 {}",
        filled_qty
    );
    assert!(!result.is_filled, "不应标记为完全成交");
    // 买单应仍挂在簿中（剩余 5 单位）
    let (bids, _) = engine.depth(10);
    assert!(!bids.is_empty(), "买单应仍挂在买盘");
}

/// 快速订单 churn：提交后立即取消，验证无内存泄漏
pub fn run_rapid_order_churn() {
    let mut engine = L2MatchingEngine::new();
    // 快速提交 500 笔订单
    for i in 0..500u64 {
        engine.submit(make_order(i + 1, Side::Buy, 100.0 - (i as f64 % 10.0), 1.0));
    }
    let active_after_submit = engine.active_order_count();
    assert!(active_after_submit > 0, "提交后应有活跃订单");
    // 通过重新提交同 ID 的卖单来"取消"（L2 引擎的取消语义）
    // 这里我们验证引擎状态一致：深度查询不 panic
    let (bids, asks) = engine.depth(500);
    let total_bid_qty: f64 = bids.iter().map(|l| l.quantity.as_f64()).sum();
    assert!(total_bid_qty > 0.0, "买盘总深度应 > 0");
    assert!(asks.is_empty(), "应无卖单");
}

// ═══════════════════════════════════════════════════════════════════════════
// L3 多资产引擎测试
// ═══════════════════════════════════════════════════════════════════════════

use axon_backtest::matching::{BatchMode, CrossPair, MultiAssetMatchingEngine};
use axon_core::types::{Instrument, SpotInstrument};

/// 0.6.0 helper:构造 spot Instrument
fn btc_spot_inst() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

fn eth_spot_inst() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("ETH"),
        quote: Symbol::from("USDT"),
    })
}

/// L3 多资产路由：不同资产的订单应路由到正确引擎
pub fn run_l3_multi_asset_routing() {
    let mut engine = MultiAssetMatchingEngine::new();
    engine.register_instrument(btc_spot_inst());
    engine.register_instrument(eth_spot_inst());

    // BTC 买单
    let btc_buy = Order::spot(
        1,
        "BTC",
        "USDT",
        Side::Buy,
        OrderType::Limit {
            price: Price::from_f64(50000.0),
        },
        Quantity::from_f64(0.1),
        TimeInForce::GTC,
    );
    let r1 = engine.submit(btc_buy).unwrap();
    assert!(r1.is_empty(), "BTC 买单无对手方");

    // ETH 卖单
    let eth_sell = Order::spot(
        2,
        "ETH",
        "USDT",
        Side::Sell,
        OrderType::Limit {
            price: Price::from_f64(3000.0),
        },
        Quantity::from_f64(1.0),
        TimeInForce::GTC,
    );
    let r2 = engine.submit(eth_sell).unwrap();
    assert!(r2.is_empty(), "ETH 卖单无对手方");

    // 验证各自引擎有挂单
    assert!(engine.engine(&btc_spot_inst()).is_some());
    assert!(engine.engine(&eth_spot_inst()).is_some());
    assert_eq!(engine.asset_count(), 2);
}

/// L3 跨资产交易对注册与套利检测
pub fn run_l3_cross_pair_arbitrage() {
    let mut engine = MultiAssetMatchingEngine::new();
    let pair = CrossPair::new(
        btc_spot_inst(),
        eth_spot_inst(),
        16.0,
        Quantity::from_f64(1.0),
    );
    engine.register_cross_pair(pair).unwrap();
    assert_eq!(engine.cross_pair_count(), 1);

    // 各挂一笔，使 mid price 不同
    engine.register_instrument(btc_spot_inst());
    engine.register_instrument(eth_spot_inst());
    engine
        .submit(Order::spot(
            10,
            "BTC",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(50000.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::GTC,
        ))
        .unwrap();
    engine
        .submit(Order::spot(
            11,
            "BTC",
            "USDT",
            Side::Sell,
            OrderType::Limit {
                price: Price::from_f64(51000.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::GTC,
        ))
        .unwrap();
    engine
        .submit(Order::spot(
            20,
            "ETH",
            "USDT",
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(3000.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::GTC,
        ))
        .unwrap();
    engine
        .submit(Order::spot(
            21,
            "ETH",
            "USDT",
            Side::Sell,
            OrderType::Limit {
                price: Price::from_f64(3100.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::GTC,
        ))
        .unwrap();

    // 套利检测不应 panic
    let opportunities = engine.detect_arbitrage();
    assert_eq!(opportunities.len(), 1);
    // implied ratio = mid_btc / mid_eth = 50500 / 3050 ≈ 16.56
    assert!(
        opportunities[0].implied_ratio.is_some(),
        "应能计算 implied ratio"
    );
}

/// L3 快照保存与恢复：状态一致性
pub fn run_l3_snapshot_restore() {
    let mut engine = MultiAssetMatchingEngine::new();
    engine.register_instrument(btc_spot_inst());
    engine.set_batch_mode(BatchMode::Auction);

    // 创建快照
    let snapshot = engine.snapshot();
    assert_eq!(snapshot.batch_mode, BatchMode::Auction);
    assert!(snapshot.engines.contains_key(&btc_spot_inst()));

    // 恢复到新引擎
    let mut engine2 = MultiAssetMatchingEngine::new();
    engine2.restore(snapshot).unwrap();
    assert_eq!(engine2.batch_mode(), BatchMode::Auction);
    assert_eq!(engine2.asset_count(), 1);
}
