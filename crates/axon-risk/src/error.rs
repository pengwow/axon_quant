use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RiskError {
    #[error("circuit breaker active until {until}")]
    CircuitBreakerActive { until: i64 },

    #[error("order rejected: {reason:?}")]
    OrderRejected { reason: RiskReason },

    #[error("config invalid: {0}")]
    ConfigInvalid(String),

    #[error("overflow in risk calculation: {0}")]
    Overflow(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RiskResult {
    Allow,
    Reject(RiskReason),
    Warn(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RiskReason {
    OrderTooLarge {
        max: f64,
        actual: f64,
    },
    PositionLimitExceeded {
        instrument: String,
        limit: f64,
    },
    MaxLeverageExceeded {
        max: f64,
        actual: f64,
    },
    MaxDrawdownExceeded {
        max_pct: f64,
        current_pct: f64,
    },
    DailyPnLLimit {
        limit: f64,
        current: f64,
    },
    CircuitBreakerActive {
        until: i64,
    },
    ConcentrationTooHigh {
        instrument: String,
        pct: f64,
    },
    InsufficientMargin {
        required: f64,
        available: f64,
    },
    /// 0.6.0 新增:跨 leg 对冲对(spot + perp)净暴露超限
    LegPairNetExposureExceeded {
        /// 对冲对 label(spot|perp)
        pair: String,
        /// 当前净暴露(spot + perp × hedge_ratio)
        current: f64,
        /// 配置上限(abs)
        limit: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
    Emergency,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAlert {
    pub severity: AlertSeverity,
    pub reason: RiskReason,
    pub timestamp: i64,
}
