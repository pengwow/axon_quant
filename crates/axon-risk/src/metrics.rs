use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskMetrics {
    pub total_exposure: f64,
    pub leverage: f64,
    pub current_drawdown: f64,
    pub daily_realized_pnl: f64,
    pub var_95: f64,
    pub concentration: HashMap<String, f64>,
}
