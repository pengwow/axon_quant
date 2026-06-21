use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
    Emergency,
}

/// 告警规则：阈值告警与缺失告警
///
/// - `Threshold`：指标值触发阈值时产生事件
/// - `Missing`：指标在指定超时窗口内未出现时产生事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertRule {
    Threshold {
        metric_name: String,
        condition: ThresholdCondition,
        severity: AlertSeverity,
        message: String,
    },
    /// 缺失检测：metric 在 `timeout_secs` 秒内未被 `MetricsRegistry::check_alerts` 报告时触发
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
    /// 阈值告警检测：每次指标上报时调用
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
            // Missing 规则不基于单次 value 评估，由 `check_missing` 处理
            AlertRule::Missing { .. } => None,
        }
    }

    /// 缺失告警检测：传入指标最后出现时间与当前时间，超出 `timeout_secs` 则触发
    ///
    /// - `metric_name`：被检测指标名（不匹配规则时返回 None）
    /// - `last_seen`：指标最近一次 `check_alerts` 时间；`None` 表示从未上报
    /// - `now`：当前时间（由调用方注入，便于测试）
    pub fn check_missing(
        &self,
        metric_name: &str,
        last_seen: Option<std::time::Instant>,
        now: std::time::Instant,
    ) -> Option<AlertEvent> {
        match self {
            AlertRule::Missing {
                metric_name: name,
                timeout_secs,
                severity,
            } => {
                if name != metric_name {
                    return None;
                }
                let stale = match last_seen {
                    // 从未上报过：使用 now 作为基准（必然 stale）
                    None => true,
                    Some(t) => now.duration_since(t).as_secs() >= *timeout_secs,
                };
                if stale {
                    let age_secs = last_seen
                        .map(|t| now.duration_since(t).as_secs())
                        .unwrap_or(u64::MAX);
                    Some(AlertEvent {
                        rule_name: name.clone(),
                        severity: *severity,
                        message: format!(
                            "metric {} missing for >{}s (actual: {}s)",
                            name, timeout_secs, age_secs
                        ),
                        value: age_secs as f64,
                        timestamp: now_unix_secs(),
                    })
                } else {
                    None
                }
            }
            // 阈值规则不在缺失路径评估
            AlertRule::Threshold { .. } => None,
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
    use std::time::{Duration, Instant};

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

    // ===== Missing 规则测试 =====

    #[test]
    fn test_missing_alert_fires_when_stale() {
        // 指标 60s 前已上报，超时 10s 必然触发
        let rule = AlertRule::Missing {
            metric_name: "heartbeat".into(),
            timeout_secs: 10,
            severity: AlertSeverity::Critical,
        };
        let now = Instant::now();
        let last_seen = Some(now - Duration::from_secs(60));
        let event = rule.check_missing("heartbeat", last_seen, now);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.severity, AlertSeverity::Critical);
        assert!(event.message.contains("heartbeat"));
        assert!(event.value >= 60.0);
    }

    #[test]
    fn test_missing_alert_fires_when_never_seen() {
        // 从未上报（last_seen = None）必然触发
        let rule = AlertRule::Missing {
            metric_name: "queue_depth".into(),
            timeout_secs: 30,
            severity: AlertSeverity::Warning,
        };
        let now = Instant::now();
        let event = rule.check_missing("queue_depth", None, now);
        assert!(event.is_some());
        assert_eq!(event.unwrap().severity, AlertSeverity::Warning);
    }

    #[test]
    fn test_missing_alert_does_not_fire_when_fresh() {
        // 指标刚上报，2s 远小于超时 10s
        let rule = AlertRule::Missing {
            metric_name: "heartbeat".into(),
            timeout_secs: 10,
            severity: AlertSeverity::Critical,
        };
        let now = Instant::now();
        let last_seen = Some(now - Duration::from_secs(2));
        let event = rule.check_missing("heartbeat", last_seen, now);
        assert!(event.is_none());
    }

    #[test]
    fn test_missing_alert_ignores_unknown_metric() {
        // 规则 metric_name 与传入的 metric_name 不一致 -> 不触发
        let rule = AlertRule::Missing {
            metric_name: "heartbeat".into(),
            timeout_secs: 10,
            severity: AlertSeverity::Critical,
        };
        let now = Instant::now();
        let last_seen = Some(now - Duration::from_secs(60));
        let event = rule.check_missing("other_metric", last_seen, now);
        assert!(event.is_none());
    }

    #[test]
    fn test_threshold_rule_ignores_missing_path() {
        // 阈值规则走 check_missing 时必须返回 None
        let rule = AlertRule::Threshold {
            metric_name: "latency".into(),
            condition: ThresholdCondition::GreaterThan(100.0),
            severity: AlertSeverity::Warning,
            message: "msg".into(),
        };
        let now = Instant::now();
        let event = rule.check_missing("latency", None, now);
        assert!(event.is_none());
    }

    #[test]
    fn test_threshold_less_than() {
        let rule = AlertRule::Threshold {
            metric_name: "balance".into(),
            condition: ThresholdCondition::LessThan(1000.0),
            severity: AlertSeverity::Critical,
            message: "balance below minimum".into(),
        };
        let event = rule.check("balance", 500.0);
        assert!(event.is_some());
        assert_eq!(event.unwrap().severity, AlertSeverity::Critical);
    }

    #[test]
    fn test_threshold_less_than_no_fire() {
        let rule = AlertRule::Threshold {
            metric_name: "balance".into(),
            condition: ThresholdCondition::LessThan(1000.0),
            severity: AlertSeverity::Critical,
            message: "balance below minimum".into(),
        };
        let event = rule.check("balance", 2000.0);
        assert!(event.is_none());
    }

    #[test]
    fn test_threshold_equal() {
        // 测试边界值：刚好等于阈值时不触发（GreaterThan 是严格大于）
        let rule = AlertRule::Threshold {
            metric_name: "count".into(),
            condition: ThresholdCondition::GreaterThan(42.0),
            severity: AlertSeverity::Info,
            message: "count reached target".into(),
        };
        let event = rule.check("count", 42.0);
        assert!(event.is_none());
    }

    #[test]
    fn test_threshold_greater_than_boundary() {
        let rule = AlertRule::Threshold {
            metric_name: "count".into(),
            condition: ThresholdCondition::GreaterThan(10.0),
            severity: AlertSeverity::Info,
            message: "count high".into(),
        };
        // 刚好等于边界不触发
        assert!(rule.check("count", 10.0).is_none());
        // 超过边界触发
        assert!(rule.check("count", 10.1).is_some());
    }

    #[test]
    fn test_threshold_less_than_boundary() {
        let rule = AlertRule::Threshold {
            metric_name: "balance".into(),
            condition: ThresholdCondition::LessThan(100.0),
            severity: AlertSeverity::Warning,
            message: "balance low".into(),
        };
        // 刚好等于边界不触发
        assert!(rule.check("balance", 100.0).is_none());
        // 低于边界触发
        assert!(rule.check("balance", 99.9).is_some());
    }

    #[test]
    fn test_alert_severity_ordering() {
        assert!(AlertSeverity::Critical as u8 > AlertSeverity::Warning as u8);
        assert!(AlertSeverity::Warning as u8 > AlertSeverity::Info as u8);
    }

    #[test]
    fn test_alert_event_fields() {
        let rule = AlertRule::Threshold {
            metric_name: "test".into(),
            condition: ThresholdCondition::GreaterThan(10.0),
            severity: AlertSeverity::Warning,
            message: "test alert".into(),
        };
        let event = rule.check("test", 20.0).unwrap();
        assert_eq!(event.rule_name, "test");
        assert_eq!(event.severity, AlertSeverity::Warning);
        assert!(event.message.contains("test alert"));
        assert_eq!(event.value, 20.0);
    }
}
