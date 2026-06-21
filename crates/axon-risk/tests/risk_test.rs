//! axon-risk 端到端测试

use axon_risk::{AlertSeverity, RiskError, RiskReason, RiskResult};

// ═══════════════════════════════════════════════════════════════════════════
// RiskResult 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_risk_result_allow() {
    let result = RiskResult::Allow;
    assert!(matches!(result, RiskResult::Allow));
}

#[test]
fn test_risk_result_reject() {
    let result = RiskResult::Reject(RiskReason::OrderTooLarge {
        max: 1000.0,
        actual: 2000.0,
    });
    assert!(matches!(result, RiskResult::Reject(_)));
}

#[test]
fn test_risk_result_warn() {
    let result = RiskResult::Warn("approaching limit".into());
    assert!(matches!(result, RiskResult::Warn(_)));
}

#[test]
fn test_risk_result_serialization() {
    let results = vec![
        RiskResult::Allow,
        RiskResult::Reject(RiskReason::OrderTooLarge {
            max: 1000.0,
            actual: 2000.0,
        }),
        RiskResult::Warn("warning".into()),
    ];
    for result in results {
        let json = serde_json::to_string(&result).unwrap();
        let restored: RiskResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// RiskReason 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_risk_reason_order_too_large() {
    let reason = RiskReason::OrderTooLarge {
        max: 1000.0,
        actual: 2000.0,
    };
    let json = serde_json::to_string(&reason).unwrap();
    let restored: RiskReason = serde_json::from_str(&json).unwrap();
    assert_eq!(reason, restored);
}

#[test]
fn test_risk_reason_position_limit_exceeded() {
    let reason = RiskReason::PositionLimitExceeded {
        instrument: "BTCUSDT".into(),
        limit: 10.0,
    };
    let json = serde_json::to_string(&reason).unwrap();
    let restored: RiskReason = serde_json::from_str(&json).unwrap();
    assert_eq!(reason, restored);
}

#[test]
fn test_risk_reason_max_leverage_exceeded() {
    let reason = RiskReason::MaxLeverageExceeded {
        max: 10.0,
        actual: 20.0,
    };
    let json = serde_json::to_string(&reason).unwrap();
    let restored: RiskReason = serde_json::from_str(&json).unwrap();
    assert_eq!(reason, restored);
}

#[test]
fn test_risk_reason_max_drawdown_exceeded() {
    let reason = RiskReason::MaxDrawdownExceeded {
        max_pct: 0.1,
        current_pct: 0.15,
    };
    let json = serde_json::to_string(&reason).unwrap();
    let restored: RiskReason = serde_json::from_str(&json).unwrap();
    assert_eq!(reason, restored);
}

#[test]
fn test_risk_reason_daily_pnl_limit() {
    let reason = RiskReason::DailyPnLLimit {
        limit: -1000.0,
        current: -1500.0,
    };
    let json = serde_json::to_string(&reason).unwrap();
    let restored: RiskReason = serde_json::from_str(&json).unwrap();
    assert_eq!(reason, restored);
}

#[test]
fn test_risk_reason_circuit_breaker_active() {
    let reason = RiskReason::CircuitBreakerActive { until: 1234567890 };
    let json = serde_json::to_string(&reason).unwrap();
    let restored: RiskReason = serde_json::from_str(&json).unwrap();
    assert_eq!(reason, restored);
}

#[test]
fn test_risk_reason_concentration_too_high() {
    let reason = RiskReason::ConcentrationTooHigh {
        instrument: "BTCUSDT".into(),
        pct: 0.5,
    };
    let json = serde_json::to_string(&reason).unwrap();
    let restored: RiskReason = serde_json::from_str(&json).unwrap();
    assert_eq!(reason, restored);
}

#[test]
fn test_risk_reason_insufficient_margin() {
    let reason = RiskReason::InsufficientMargin {
        required: 10000.0,
        available: 5000.0,
    };
    let json = serde_json::to_string(&reason).unwrap();
    let restored: RiskReason = serde_json::from_str(&json).unwrap();
    assert_eq!(reason, restored);
}

// ═══════════════════════════════════════════════════════════════════════════
// RiskError 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_risk_error_display() {
    let errors: Vec<RiskError> = vec![
        RiskError::CircuitBreakerActive { until: 1234567890 },
        RiskError::OrderRejected {
            reason: RiskReason::OrderTooLarge {
                max: 1000.0,
                actual: 2000.0,
            },
        },
        RiskError::ConfigInvalid("invalid config".into()),
        RiskError::Overflow("overflow".into()),
    ];

    for err in errors {
        assert!(!err.to_string().is_empty());
    }
}

#[test]
fn test_risk_error_circuit_breaker_active() {
    let err = RiskError::CircuitBreakerActive { until: 1234567890 };
    assert!(err.to_string().contains("1234567890"));
}

#[test]
fn test_risk_error_order_rejected() {
    let err = RiskError::OrderRejected {
        reason: RiskReason::OrderTooLarge {
            max: 1000.0,
            actual: 2000.0,
        },
    };
    assert!(err.to_string().contains("order rejected"));
}

#[test]
fn test_risk_error_config_invalid() {
    let err = RiskError::ConfigInvalid("missing field".into());
    assert!(err.to_string().contains("missing field"));
}

#[test]
fn test_risk_error_overflow() {
    let err = RiskError::Overflow("f64 overflow".into());
    assert!(err.to_string().contains("f64 overflow"));
}

// ═══════════════════════════════════════════════════════════════════════════
// AlertSeverity 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_alert_severity_variants() {
    assert_ne!(AlertSeverity::Info, AlertSeverity::Warning);
    assert_ne!(AlertSeverity::Warning, AlertSeverity::Critical);
    assert_ne!(AlertSeverity::Critical, AlertSeverity::Emergency);
}

#[test]
fn test_alert_severity_ordering() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Critical);
    assert!(AlertSeverity::Critical < AlertSeverity::Emergency);
}

#[test]
fn test_alert_severity_serialization() {
    let severities = vec![
        AlertSeverity::Info,
        AlertSeverity::Warning,
        AlertSeverity::Critical,
        AlertSeverity::Emergency,
    ];
    for severity in severities {
        let json = serde_json::to_string(&severity).unwrap();
        let restored: AlertSeverity = serde_json::from_str(&json).unwrap();
        assert_eq!(severity, restored);
    }
}
