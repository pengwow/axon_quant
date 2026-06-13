use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub max_position_per_instrument: f64,
    pub max_total_exposure: f64,
    pub max_order_value: f64,
    pub max_leverage: f64,
    pub max_drawdown: f64,
    pub max_daily_loss: f64,
    pub max_concentration: f64,
    pub circuit_breaker_cooldown: Duration,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position_per_instrument: 100_000.0,
            max_total_exposure: 1_000_000.0,
            max_order_value: 50_000.0,
            max_leverage: 5.0,
            max_drawdown: 0.15,
            max_daily_loss: 10_000.0,
            max_concentration: 0.40,
            circuit_breaker_cooldown: Duration::from_secs(3600),
        }
    }
}
