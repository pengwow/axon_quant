use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
    Emergency,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertRule {
    Threshold {
        metric_name: String,
        condition: ThresholdCondition,
        severity: AlertSeverity,
        message: String,
    },
    Missing {
        metric_name: String,
        timeout_secs: u64,
        severity: AlertSeverity,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ThresholdCondition {
    GreaterThan(f64),
    LessThan(f64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertEvent {
    pub rule_name: String,
    pub severity: AlertSeverity,
    pub message: String,
    pub value: f64,
    pub timestamp: i64,
}

impl AlertRule {
    pub fn check(&self, metric_name: &str, value: f64) -> Option<AlertEvent> {
        match self {
            AlertRule::Threshold {
                metric_name: name,
                condition,
                severity,
                message,
            } => {
                if name != metric_name {
                    return None;
                }
                let triggered = match condition {
                    ThresholdCondition::GreaterThan(threshold) => value > *threshold,
                    ThresholdCondition::LessThan(threshold) => value < *threshold,
                };
                if triggered {
                    Some(AlertEvent {
                        rule_name: name.clone(),
                        severity: *severity,
                        message: message.clone(),
                        value,
                        timestamp: now_unix_secs(),
                    })
                } else {
                    None
                }
            }
            AlertRule::Missing { .. } => None,
        }
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

    #[test]
    fn test_threshold_alert_fires() {
        let rule = AlertRule::Threshold {
            metric_name: "latency".into(),
            condition: ThresholdCondition::GreaterThan(10_000_000.0),
            severity: AlertSeverity::Warning,
            message: "latency exceeds 10ms".into(),
        };
        let event = rule.check("latency", 50_000_000.0);
        assert!(event.is_some());
        assert_eq!(event.unwrap().severity, AlertSeverity::Warning);
    }

    #[test]
    fn test_threshold_alert_no_fire() {
        let rule = AlertRule::Threshold {
            metric_name: "latency".into(),
            condition: ThresholdCondition::GreaterThan(10_000_000.0),
            severity: AlertSeverity::Warning,
            message: "latency exceeds 10ms".into(),
        };
        let event = rule.check("latency", 5_000_000.0);
        assert!(event.is_none());
    }

    #[test]
    fn test_threshold_wrong_metric() {
        let rule = AlertRule::Threshold {
            metric_name: "latency".into(),
            condition: ThresholdCondition::GreaterThan(10_000_000.0),
            severity: AlertSeverity::Warning,
            message: "latency exceeds 10ms".into(),
        };
        let event = rule.check("throughput", 50_000_000.0);
        assert!(event.is_none());
    }
}
