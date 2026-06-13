use axon_core::market::Side;
use axon_core::order::Order;
use axon_core::portfolio::Portfolio;

use crate::config::RiskConfig;
use crate::error::{RiskReason, RiskResult};

pub fn check_position_limit(
    order: &Order,
    portfolio: &Portfolio,
    config: &RiskConfig,
) -> RiskResult {
    let current_qty = portfolio
        .positions()
        .get(&order.symbol)
        .map(|p| p.quantity.as_f64())
        .unwrap_or(0.0);

    let delta = match order.side {
        Side::Buy => order.quantity.as_f64(),
        Side::Sell => -order.quantity.as_f64(),
    };
    let new_qty = (current_qty + delta).abs();

    if new_qty > config.max_position_per_instrument {
        return RiskResult::Reject(RiskReason::PositionLimitExceeded {
            instrument: order.symbol.to_string(),
            limit: config.max_position_per_instrument,
        });
    }
    RiskResult::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::order::{OrderType, TimeInForce};
    use axon_core::types::{Price, Quantity, Symbol};

    fn make_order(side: Side, qty: f64) -> Order {
        Order::new(
            1,
            Symbol::from("BTC-USDT"),
            side,
            OrderType::Limit {
                price: Price::from_f64(100.0),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        )
    }

    fn empty_portfolio() -> Portfolio {
        Portfolio::default()
    }

    #[test]
    fn test_position_limit_no_existing_position() {
        let config = RiskConfig::default(); // max = 100_000
        let order = make_order(Side::Buy, 50_000.0);
        assert_eq!(
            check_position_limit(&order, &empty_portfolio(), &config),
            RiskResult::Allow
        );
    }

    #[test]
    fn test_position_limit_exceeded() {
        let config = RiskConfig {
            max_position_per_instrument: 100.0,
            ..Default::default()
        };
        let order = make_order(Side::Buy, 150.0);
        assert!(matches!(
            check_position_limit(&order, &empty_portfolio(), &config),
            RiskResult::Reject(RiskReason::PositionLimitExceeded { .. })
        ));
    }
}
