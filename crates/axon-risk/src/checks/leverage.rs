use axon_core::portfolio::Portfolio;

use crate::config::RiskConfig;
use crate::error::{RiskReason, RiskResult};

pub fn check_leverage(portfolio: &Portfolio, config: &RiskConfig) -> RiskResult {
    let cash = portfolio.base_cash();
    if cash <= 0.0 {
        return RiskResult::Reject(RiskReason::InsufficientMargin {
            required: 0.0,
            available: cash,
        });
    }
    let nav = portfolio.nav() as f64 / 1_000_000.0;
    let leverage = nav / cash;
    if leverage > config.max_leverage {
        return RiskResult::Reject(RiskReason::MaxLeverageExceeded {
            max: config.max_leverage,
            actual: leverage,
        });
    }
    RiskResult::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::portfolio::Currency;

    #[test]
    fn test_leverage_within_limit() {
        let mut portfolio = Portfolio::new(Currency::USD, 0.001);
        portfolio.deposit(Currency::USD, 100_000.0);
        let config = RiskConfig::default(); // max_leverage = 5.0
        assert_eq!(check_leverage(&portfolio, &config), RiskResult::Allow);
    }

    #[test]
    fn test_leverage_no_cash() {
        let portfolio = Portfolio::new(Currency::USD, 0.001);
        let config = RiskConfig::default();
        assert!(matches!(
            check_leverage(&portfolio, &config),
            RiskResult::Reject(RiskReason::InsufficientMargin { .. })
        ));
    }
}
