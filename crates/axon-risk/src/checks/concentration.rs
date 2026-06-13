use axon_core::portfolio::Portfolio;

use crate::config::RiskConfig;
use crate::error::{AlertSeverity, RiskAlert, RiskReason};

pub fn check_concentration(portfolio: &Portfolio, config: &RiskConfig) -> Vec<RiskAlert> {
    let mut alerts = Vec::new();
    let nav = portfolio.nav() as f64 / 1_000_000.0;
    if nav <= 0.0 {
        return alerts;
    }
    for (symbol, position) in portfolio.positions() {
        let mv = match position.market_value() {
            Some(v) => v as f64 / 1_000_000.0,
            None => continue,
        };
        let pct = mv / nav;
        if pct > config.max_concentration {
            alerts.push(RiskAlert {
                severity: AlertSeverity::Warning,
                reason: RiskReason::ConcentrationTooHigh {
                    instrument: symbol.to_string(),
                    pct,
                },
                timestamp: now_unix_secs(),
            });
        }
    }
    alerts
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::portfolio::Currency;

    #[test]
    fn test_concentration_within_limit() {
        let mut portfolio = Portfolio::new(Currency::USD, 0.001);
        portfolio.deposit(Currency::USD, 100_000.0);
        let config = RiskConfig::default();
        let alerts = check_concentration(&portfolio, &config);
        assert!(alerts.is_empty());
    }

    #[test]
    fn test_concentration_empty_portfolio() {
        let portfolio = Portfolio::default();
        let config = RiskConfig::default();
        let alerts = check_concentration(&portfolio, &config);
        assert!(alerts.is_empty());
    }
}
