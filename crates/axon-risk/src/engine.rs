use std::collections::HashMap;

use axon_core::order::Order;
use axon_core::portfolio::Portfolio;
use parking_lot::Mutex;

use crate::checks::{concentration, drawdown, leverage, order_size, position};
use crate::circuit_breaker::CircuitBreaker;
use crate::config::RiskConfig;
use crate::error::{AlertSeverity, RiskAlert, RiskReason, RiskResult};
use crate::metrics::RiskMetrics;

pub trait RiskEngine: Send + Sync {
    fn check_order(&self, order: &Order, portfolio: &Portfolio) -> RiskResult;
    fn check_portfolio(&self, portfolio: &Portfolio) -> Vec<RiskAlert>;
    fn update_daily_pnl(&self, pnl: f64);
    fn get_metrics(&self, portfolio: &Portfolio) -> RiskMetrics;
    fn reset_daily(&self);
}

pub struct DefaultRiskEngine {
    config: RiskConfig,
    circuit_breaker: CircuitBreaker,
    daily_pnl: Mutex<f64>,
    peak_value: Mutex<f64>,
}

impl DefaultRiskEngine {
    pub fn new(config: RiskConfig) -> Self {
        let cb = CircuitBreaker::new(config.max_daily_loss, config.circuit_breaker_cooldown);
        Self {
            config,
            circuit_breaker: cb,
            daily_pnl: Mutex::new(0.0),
            peak_value: Mutex::new(0.0),
        }
    }
}

impl RiskEngine for DefaultRiskEngine {
    fn check_order(&self, order: &Order, portfolio: &Portfolio) -> RiskResult {
        if self.circuit_breaker.is_active() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            return RiskResult::Reject(RiskReason::CircuitBreakerActive {
                until: now + self.config.circuit_breaker_cooldown.as_secs() as i64,
            });
        }

        let r = order_size::check_order_size(order, &self.config);
        if !matches!(r, RiskResult::Allow) {
            return r;
        }

        let r = position::check_position_limit(order, portfolio, &self.config);
        if !matches!(r, RiskResult::Allow) {
            return r;
        }

        let r = leverage::check_leverage(portfolio, &self.config);
        if !matches!(r, RiskResult::Allow) {
            return r;
        }

        let peak = *self.peak_value.lock();
        drawdown::check_drawdown(portfolio, peak, &self.config)
    }

    fn check_portfolio(&self, portfolio: &Portfolio) -> Vec<RiskAlert> {
        let mut alerts = Vec::new();

        let daily_pnl = *self.daily_pnl.lock();
        if daily_pnl <= -self.config.max_daily_loss {
            alerts.push(RiskAlert {
                severity: AlertSeverity::Emergency,
                reason: RiskReason::DailyPnLLimit {
                    limit: self.config.max_daily_loss,
                    current: daily_pnl,
                },
                timestamp: now_unix_secs(),
            });
        }

        alerts.extend(concentration::check_concentration(portfolio, &self.config));
        alerts
    }

    fn update_daily_pnl(&self, pnl: f64) {
        let mut current = self.daily_pnl.lock();
        *current += pnl;
        self.circuit_breaker.check_and_trigger(*current);

        let nav = pnl; // simplified: caller should pass net PnL
        let mut peak = self.peak_value.lock();
        if nav > *peak {
            *peak = nav;
        }
    }

    fn get_metrics(&self, portfolio: &Portfolio) -> RiskMetrics {
        let nav = portfolio.nav() as f64 / 1_000_000.0;
        let cash = portfolio.base_cash();
        let leverage_val = if cash > 0.0 {
            nav / cash
        } else {
            f64::INFINITY
        };

        let mut concentration_map = HashMap::new();
        if nav > 0.0 {
            for (symbol, pos) in portfolio.positions() {
                if let Some(mv) = pos.market_value() {
                    concentration_map.insert(symbol.to_string(), mv as f64 / 1_000_000.0 / nav);
                }
            }
        }

        let peak = *self.peak_value.lock();
        let current_drawdown = if peak > 0.0 { (peak - nav) / peak } else { 0.0 };

        RiskMetrics {
            total_exposure: nav,
            leverage: leverage_val,
            current_drawdown,
            daily_realized_pnl: *self.daily_pnl.lock(),
            var_95: 0.0,
            concentration: concentration_map,
        }
    }

    fn reset_daily(&self) {
        *self.daily_pnl.lock() = 0.0;
        self.circuit_breaker.reset();
    }
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
    use axon_core::market::Side;
    use axon_core::order::{OrderType, TimeInForce};
    use axon_core::portfolio::Currency;
    use axon_core::types::{Price, Quantity, Symbol};

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
    fn test_check_order_allows_valid() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 10.0);
        assert_eq!(engine.check_order(&order, &portfolio), RiskResult::Allow);
    }

    #[test]
    fn test_check_order_rejects_circuit_breaker() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        engine.update_daily_pnl(-10_000.0);
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 10.0);
        assert!(matches!(
            engine.check_order(&order, &portfolio),
            RiskResult::Reject(RiskReason::CircuitBreakerActive { .. })
        ));
    }

    #[test]
    fn test_check_order_rejects_oversized() {
        let config = RiskConfig {
            max_order_value: 1_000.0,
            ..Default::default()
        };
        let engine = DefaultRiskEngine::new(config);
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 20.0); // value = 2_000
        assert!(matches!(
            engine.check_order(&order, &portfolio),
            RiskResult::Reject(RiskReason::OrderTooLarge { .. })
        ));
    }

    #[test]
    fn test_check_order_short_circuit() {
        let config = RiskConfig {
            max_order_value: 1.0,
            max_position_per_instrument: 0.001,
            ..Default::default()
        };
        let engine = DefaultRiskEngine::new(config);
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 10.0);
        // Should reject at order_size check (step 2), not position check (step 3)
        assert!(matches!(
            engine.check_order(&order, &portfolio),
            RiskResult::Reject(RiskReason::OrderTooLarge { .. })
        ));
    }

    #[test]
    fn test_update_daily_pnl() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        engine.update_daily_pnl(5_000.0);
        engine.update_daily_pnl(-3_000.0);
        let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
        assert_eq!(metrics.daily_realized_pnl, 2_000.0);
    }

    #[test]
    fn test_reset_daily() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        engine.update_daily_pnl(-9_000.0);
        engine.reset_daily();
        let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
        assert_eq!(metrics.daily_realized_pnl, 0.0);
    }

    #[test]
    fn test_get_metrics() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        let portfolio = funded_portfolio(100_000.0);
        let metrics = engine.get_metrics(&portfolio);
        assert!(metrics.leverage > 0.0);
        assert!(metrics.concentration.is_empty());
    }
}
