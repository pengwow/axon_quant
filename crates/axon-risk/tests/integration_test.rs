use std::sync::Arc;

use axon_core::market::Side;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::portfolio::{Currency, Portfolio};
use axon_core::types::{Price, Quantity, Symbol};

use axon_risk::{DefaultRiskEngine, RiskConfig, RiskEngine, RiskReason, RiskResult};

fn make_limit_order(side: Side, price: f64, qty: f64) -> Order {
    Order::new(
        1,
        Symbol::from("BTC-USDT"),
        side,
        OrderType::Limit {
            price: Price::from_f64(price),
        },
        Quantity::from_f64(qty),
        TimeInForce::GTC,
    )
}

fn funded_portfolio(cash: f64) -> Portfolio {
    let mut p = Portfolio::new(Currency::USD, 0.001);
    p.deposit(Currency::USD, cash);
    p
}

#[test]
fn test_full_risk_check_flow() {
    let engine = DefaultRiskEngine::new(RiskConfig::default());
    let portfolio = funded_portfolio(100_000.0);

    // Valid order passes
    let order = make_limit_order(Side::Buy, 100.0, 10.0);
    assert_eq!(engine.check_order(&order, &portfolio), RiskResult::Allow);

    // Update PnL to approach limit
    engine.update_daily_pnl(-8_000.0);

    // Still passes
    let order = make_limit_order(Side::Buy, 100.0, 10.0);
    assert_eq!(engine.check_order(&order, &portfolio), RiskResult::Allow);

    // Push over daily loss limit
    engine.update_daily_pnl(-3_000.0);

    // Now circuit breaker should reject
    let order = make_limit_order(Side::Buy, 100.0, 10.0);
    assert!(matches!(
        engine.check_order(&order, &portfolio),
        RiskResult::Reject(RiskReason::CircuitBreakerActive { .. })
    ));

    // Reset and verify recovery
    engine.reset_daily();
    assert_eq!(engine.check_order(&order, &portfolio), RiskResult::Allow);
}

#[test]
fn test_order_lifecycle_with_risk() {
    let config = RiskConfig {
        max_order_value: 5_000.0,
        max_position_per_instrument: 50.0,
        ..Default::default()
    };
    let engine = DefaultRiskEngine::new(config);
    let portfolio = funded_portfolio(100_000.0);

    // Small order passes
    let order1 = make_limit_order(Side::Buy, 100.0, 10.0); // value = 1_000
    assert_eq!(engine.check_order(&order1, &portfolio), RiskResult::Allow);

    // Large order rejected
    let order2 = make_limit_order(Side::Buy, 100.0, 60.0); // value = 6_000
    assert!(matches!(
        engine.check_order(&order2, &portfolio),
        RiskResult::Reject(RiskReason::OrderTooLarge { .. })
    ));
}

#[test]
fn test_concurrent_check_order() {
    let engine = Arc::new(DefaultRiskEngine::new(RiskConfig::default()));
    let portfolio = Arc::new(funded_portfolio(1_000_000.0));

    let mut handles = vec![];
    for i in 0..10 {
        let engine = engine.clone();
        let portfolio = portfolio.clone();
        handles.push(std::thread::spawn(move || {
            let order = make_limit_order(Side::Buy, 100.0, i as f64 + 1.0);
            engine.check_order(&order, &portfolio)
        }));
    }

    for handle in handles {
        let result = handle.join().unwrap();
        assert_eq!(result, RiskResult::Allow);
    }
}

#[test]
fn test_concurrent_update_pnl() {
    let engine = Arc::new(DefaultRiskEngine::new(RiskConfig::default()));

    let mut handles = vec![];
    for _ in 0..10 {
        let engine = engine.clone();
        handles.push(std::thread::spawn(move || {
            engine.update_daily_pnl(-100.0);
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
    assert_eq!(metrics.daily_realized_pnl, -1_000.0);
}
