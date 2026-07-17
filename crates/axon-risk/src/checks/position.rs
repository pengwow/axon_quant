use axon_core::market::Side;
use axon_core::order::Order;
use axon_core::portfolio::Portfolio;
use axon_core::types::{Instrument, Symbol};

use crate::config::RiskConfig;
use crate::error::{RiskReason, RiskResult};

#[inline]
pub fn check_position_limit(
    order: &Order,
    portfolio: &Portfolio,
    config: &RiskConfig,
) -> RiskResult {
    // portfolio.positions() keyed by Symbol; convert via key string for
    // compat with existing Portfolio API (T2.2 transitional state).
    let key = instrument_to_key(&order.instrument);
    let current_qty = portfolio
        .positions()
        .get(&Symbol::from(key))
        .map(|p| p.quantity.as_f64())
        .unwrap_or(0.0);

    let delta = match order.side {
        Side::Buy => order.quantity.as_f64(),
        Side::Sell => -order.quantity.as_f64(),
    };
    let new_qty = (current_qty + delta).abs();

    if new_qty > config.max_position_per_instrument {
        return RiskResult::Reject(RiskReason::PositionLimitExceeded {
            instrument: format!(
                "{}/{}",
                order.instrument.base().as_str(),
                order.instrument.quote().as_str()
            ),
            limit: config.max_position_per_instrument,
        });
    }
    RiskResult::Allow
}

/// Transitionally convert an Instrument to the BASE/QUOTE String used by
/// `Portfolio.positions()` keys. T3.5 will replace with direct Instrument key.
fn instrument_to_key(inst: &Instrument) -> String {
    format!("{}/{}", inst.base().as_str(), inst.quote().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::order::{OrderType, TimeInForce};
    use axon_core::types::{Price, Quantity};

    fn make_order(side: Side, qty: f64) -> Order {
        Order::spot(
            1,
            "BTC",
            "USDT",
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
