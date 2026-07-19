//! Phase 3.1.4 集成测试:`Box<dyn MatchingEngine>` 多态验证
//!
//! 目的:验证 L1 / L2 / Impacted / MultiAsset 四个撮合引擎实现
//! `MatchingEngine` trait 后,能装入 `Box<dyn MatchingEngine>` 多态使用。
//!
//! 验收:
//! - 每个引擎都能 `Box<dyn MatchingEngine>::submit` / `cancel` / `best_bid` 等
//! - trait 多态下,同 group of orders 跑出等价的 fills 数(允许 0 成交,
//!   因为多资产未 with_primary 时 best_bid 返回 None)
//!
//! 运行:`cargo test -p axon-backtest --test trait_polymorphism`

use axon_backtest::impact::ImpactedMatchingEngine;
use axon_backtest::matching::{
    L1MatchingEngine, L2MatchingEngine, MatchingEngine, MultiAssetMatchingEngine,
};
use axon_core::impact::LinearImpactModel;
use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Instrument, Price, Quantity, SpotInstrument, Symbol};

/// 构造一个 spot BTC/USDT 限价单
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

fn btc_spot() -> Instrument {
    Instrument::Spot(SpotInstrument {
        base: Symbol::from("BTC"),
        quote: Symbol::from("USDT"),
    })
}

/// 场景:挂 sell@100,挂 buy@100,验证 1 笔成交
/// 四个引擎跑同一 group,断言 fills 数 == 1
fn run_polymorphic_scenario(make_engine: impl FnOnce() -> Box<dyn MatchingEngine>) -> usize {
    let mut engine = make_engine();

    // 卖单
    let sell = make_limit_order(1, Side::Sell, 100.0, 1.0);
    engine.submit(sell);

    // 买单(应成交)
    let buy = make_limit_order(2, Side::Buy, 100.0, 1.0);
    let result = engine.submit(buy);
    result.fills.len()
}

// ─── L1:多态装入 trait object ───────────────────────────────

#[test]
fn l1_engine_works_as_dyn_matching_engine() {
    let engine: Box<dyn MatchingEngine> = Box::new(L1MatchingEngine::new());
    assert!(engine.best_bid().is_none());
    assert!(engine.best_ask().is_none());
    assert_eq!(engine.active_order_count(), 0);
    assert_eq!(engine.spread(), None);
}

// ─── L2:多态装入 trait object ───────────────────────────────

#[test]
fn l2_engine_works_as_dyn_matching_engine() {
    let engine: Box<dyn MatchingEngine> = Box::new(L2MatchingEngine::new());
    assert!(engine.best_bid().is_none());
    assert!(engine.best_ask().is_none());
    assert_eq!(engine.active_order_count(), 0);
}

// ─── Impacted:多态装入 trait object ─────────────────────────

#[test]
fn impacted_engine_works_as_dyn_matching_engine() {
    let model = Box::new(LinearImpactModel::default());
    let engine: Box<dyn MatchingEngine> = Box::new(ImpactedMatchingEngine::new(model));
    assert!(engine.best_bid().is_none());
    assert!(engine.best_ask().is_none());
    assert_eq!(engine.active_order_count(), 0);
}

// ─── MultiAsset:多态装入 trait object(需 with_primary) ──────

#[test]
fn multi_asset_engine_works_as_dyn_matching_engine() {
    let engine = MultiAssetMatchingEngine::new().with_primary(btc_spot());
    let mut engine: Box<dyn MatchingEngine> = Box::new(engine);

    // primary 未 seed ⇒ best_* 为 None
    assert!(engine.best_bid().is_none());
    assert!(engine.best_ask().is_none());

    // seed 后 best_* 应有值
    let _ = engine.seed_liquidity(100.0, 0.5, 2, 1.0, btc_spot(), 1);
    assert!(engine.best_bid().is_some());
    assert!(engine.best_ask().is_some());
    assert_eq!(engine.active_order_count(), 4); // 2 卖 + 2 买
}

// ─── 跨 trait 多态:统一函数跑同一 scenario ──────────────

#[test]
fn polymorphic_scenario_l1_fills_one() {
    let fills = run_polymorphic_scenario(|| Box::new(L1MatchingEngine::new()));
    assert_eq!(fills, 1, "L1 应成交 1 笔");
}

#[test]
fn polymorphic_scenario_l2_fills_one() {
    let fills = run_polymorphic_scenario(|| Box::new(L2MatchingEngine::new()));
    assert_eq!(fills, 1, "L2 应成交 1 笔");
}

#[test]
fn polymorphic_scenario_impacted_fills_one() {
    let model = Box::new(LinearImpactModel::default());
    let fills = run_polymorphic_scenario(|| Box::new(ImpactedMatchingEngine::new(model)));
    assert_eq!(fills, 1, "Impacted 应成交 1 笔");
}

#[test]
fn polymorphic_scenario_multi_asset_fills_one() {
    let engine = MultiAssetMatchingEngine::new().with_primary(btc_spot());
    let fills = run_polymorphic_scenario(|| Box::new(engine));
    assert_eq!(fills, 1, "MultiAsset 应成交 1 笔(primary 路由)");
}

// ─── 取消 + 清空多态 ─────────────────────────────────

#[test]
fn polymorphic_cancel_and_clear() {
    let mut engine: Box<dyn MatchingEngine> = Box::new(L2MatchingEngine::new());
    engine.submit(make_limit_order(1, Side::Sell, 100.0, 1.0));
    engine.submit(make_limit_order(2, Side::Sell, 101.0, 1.0));
    assert_eq!(engine.active_order_count(), 2);

    // 取消 1
    assert!(engine.cancel(1));
    assert_eq!(engine.active_order_count(), 1);

    // 清空
    engine.clear_book();
    assert_eq!(engine.active_order_count(), 0);
    assert!(engine.best_bid().is_none());
    assert!(engine.best_ask().is_none());
}

#[test]
fn polymorphic_clear_book_for_isolated() {
    let eth_spot = Instrument::Spot(SpotInstrument {
        base: Symbol::from("ETH"),
        quote: Symbol::from("USDT"),
    });
    let engine = MultiAssetMatchingEngine::new().with_primary(btc_spot());
    let mut engine: Box<dyn MatchingEngine> = Box::new(engine);

    // BTC + ETH 各 seed
    let _ = engine.seed_liquidity(100.0, 0.5, 2, 1.0, btc_spot(), 1);
    let _ = engine.seed_liquidity(200.0, 0.5, 2, 1.0, eth_spot.clone(), 100);
    assert_eq!(engine.active_order_count(), 8, "BTC 4 + ETH 4");

    // 只清 BTC,ETH 应保留
    engine.clear_book_for(&btc_spot());
    assert_eq!(
        engine.active_order_count(),
        4,
        "BTC 4 笔被清,ETH 4 笔应保留"
    );
}

// ─── seed_liquidity 多态:四个引擎都能 seed ──────────────

#[test]
fn polymorphic_seed_liquidity_returns_updated_id() {
    let mut engines: Vec<(&str, Box<dyn MatchingEngine>)> = vec![
        ("L1", Box::new(L1MatchingEngine::new())),
        ("L2", Box::new(L2MatchingEngine::new())),
        (
            "Impacted",
            Box::new(ImpactedMatchingEngine::new(Box::new(
                LinearImpactModel::default(),
            ))),
        ),
        (
            "MultiAsset",
            Box::new(MultiAssetMatchingEngine::new().with_primary(btc_spot())),
        ),
    ];

    for (name, engine) in engines.iter_mut() {
        let next_id = engine.seed_liquidity(100.0, 0.5, 2, 1.0, btc_spot(), 1);
        assert_eq!(
            next_id, 5,
            "{name}: seed 2 卖 + 2 买 = 4 单,next_id 应返回 1 + 4 = 5"
        );
        assert_eq!(engine.active_order_count(), 4, "{name}: 4 笔 maker");
    }
}
