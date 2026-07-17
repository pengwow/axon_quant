use axon_core::order::Order;

use crate::config::RiskConfig;
use crate::error::RiskResult;

#[inline]
pub fn check_order_size(order: &Order, config: &RiskConfig) -> RiskResult {
    let Some(price) = order.order_type.limit_price() else {
        return RiskResult::Allow;
    };
    let order_value = price.as_f64() * order.quantity.as_f64();
    if order_value > config.max_order_value {
        return RiskResult::Reject(crate::error::RiskReason::OrderTooLarge {
            max: config.max_order_value,
            actual: order_value,
        });
    }
    RiskResult::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::Side;
    use axon_core::order::{OrderType, TimeInForce};
    use axon_core::types::{Price, Quantity};

    fn make_limit_order(price: f64, qty: f64) -> Order {
        Order::spot(
1,
"BTC",
"USDT",Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        )
    }

    fn make_market_order(qty: f64) -> Order {
        Order::spot(
1,
"BTC",
"USDT",Side::Buy,
            OrderType::Market,
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        )
    }

    #[test]
    fn test_order_size_within_limit() {
        let config = RiskConfig::default();
        let order = make_limit_order(100.0, 100.0); // value = 10_000
        assert_eq!(check_order_size(&order, &config), RiskResult::Allow);
    }

    #[test]
    fn test_order_size_exceeds_limit() {
        let config = RiskConfig::default(); // max_order_value = 50_000
        let order = make_limit_order(100.0, 600.0); // value = 60_000
        assert!(matches!(
            check_order_size(&order, &config),
            RiskResult::Reject(crate::error::RiskReason::OrderTooLarge { .. })
        ));
    }

    #[test]
    fn test_market_order_skips_size_check() {
        let config = RiskConfig::default();
        let order = make_market_order(1_000_000.0);
        assert_eq!(check_order_size(&order, &config), RiskResult::Allow);
    }
}
