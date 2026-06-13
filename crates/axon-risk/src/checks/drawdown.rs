use axon_core::portfolio::Portfolio;

use crate::config::RiskConfig;
use crate::error::{RiskReason, RiskResult};

pub fn check_drawdown(portfolio: &Portfolio, peak_value: f64, config: &RiskConfig) -> RiskResult {
    if peak_value <= 0.0 {
        return RiskResult::Allow;
    }
    let current = portfolio.nav() as f64 / 1_000_000.0;
    let drawdown = (peak_value - current) / peak_value;
    if drawdown > config.max_drawdown {
        return RiskResult::Reject(RiskReason::MaxDrawdownExceeded {
            max_pct: config.max_drawdown,
            current_pct: drawdown,
        });
    }
    if drawdown > config.max_drawdown * 0.8 {
        return RiskResult::Warn(format!(
            "drawdown approaching limit: {:.1}%",
            drawdown * 100.0
        ));
    }
    RiskResult::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::portfolio::Currency;

    #[test]
    fn test_drawdown_within_limit() {
        let mut portfolio = Portfolio::new(Currency::USD, 0.001);
        portfolio.deposit(Currency::USD, 90_000.0);
        let config = RiskConfig::default(); // max_drawdown = 0.15
        assert_eq!(
            check_drawdown(&portfolio, 100_000.0, &config),
            RiskResult::Allow
        );
    }

    #[test]
    fn test_drawdown_exceeded() {
        let mut portfolio = Portfolio::new(Currency::USD, 0.001);
        portfolio.deposit(Currency::USD, 80_000.0);
        let config = RiskConfig::default(); // max_drawdown = 0.15
        assert!(matches!(
            check_drawdown(&portfolio, 100_000.0, &config),
            RiskResult::Reject(RiskReason::MaxDrawdownExceeded { .. })
        ));
    }

    #[test]
    fn test_drawdown_warning_threshold() {
        let mut portfolio = Portfolio::new(Currency::USD, 0.001);
        portfolio.deposit(Currency::USD, 87_000.0);
        let config = RiskConfig::default(); // max_drawdown = 0.15, 80% = 0.12
        assert!(matches!(
            check_drawdown(&portfolio, 100_000.0, &config),
            RiskResult::Warn(_)
        ));
    }

    #[test]
    fn test_drawdown_zero_peak() {
        let portfolio = Portfolio::default();
        let config = RiskConfig::default();
        assert_eq!(check_drawdown(&portfolio, 0.0, &config), RiskResult::Allow);
    }
}
