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
    /// 0.6.0 新增:跨 leg 对冲对(spot + perp)允许的净暴露上限(absolute)
    ///
    /// 当 `spot_qty + perp_qty * hedge_ratio` 超过此值时,
    /// `check_leg_pair_net_exposure` 返回 Reject。默认 0.0(强制 delta 中性)。
    pub max_leg_pair_net_exposure: f64,
    /// 0.6.0 新增:跨 leg VaR 上限(单一对冲对在 95% 置信下的最大可承受损失,USD)
    pub max_leg_pair_var_95: f64,
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
            // 0.6.0 默认值:强制 delta 中性 + 单对 VaR < 5_000 USD
            max_leg_pair_net_exposure: 0.0,
            max_leg_pair_var_95: 5_000.0,
        }
    }
}
