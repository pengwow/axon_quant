use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;

use crate::alert::{AlertEvent, AlertRule};
use crate::metrics::{AtomicCounter, AtomicGauge, LatencyHistogram};

/// 指标注册中心
///
/// 维护 Counter / Gauge / Histogram 三类指标以及告警规则。
/// `check_alerts` 在评估阈值规则的同时，会把指标最后出现时间记录到 `last_seen`；
/// `check_missing_alerts` 则基于 `last_seen` + `Missing` 规则评估缺失告警。
pub struct MetricsRegistry {
    counters: HashMap<String, Arc<AtomicCounter>>,
    gauges: HashMap<String, Arc<AtomicGauge>>,
    histograms: HashMap<String, Arc<LatencyHistogram>>,
    alert_rules: Vec<AlertRule>,
    alert_events: RwLock<Vec<AlertEvent>>,
    /// 每个指标最近一次 `check_alerts` 时间，用于缺失告警检测
    last_seen: RwLock<HashMap<String, Instant>>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self {
            counters: HashMap::new(),
            gauges: HashMap::new(),
            histograms: HashMap::new(),
            alert_rules: Vec::new(),
            alert_events: RwLock::new(Vec::new()),
            last_seen: RwLock::new(HashMap::new()),
        }
    }

    pub fn register_counter(&mut self, name: &str) -> Arc<AtomicCounter> {
        let counter = Arc::new(AtomicCounter::new());
        self.counters.insert(name.to_string(), counter.clone());
        counter
    }

    pub fn register_gauge(&mut self, name: &str) -> Arc<AtomicGauge> {
        let gauge = Arc::new(AtomicGauge::new());
        self.gauges.insert(name.to_string(), gauge.clone());
        gauge
    }

    pub fn register_histogram(&mut self, name: &str) -> Arc<LatencyHistogram> {
        let hist = Arc::new(LatencyHistogram::default_latency());
        self.histograms.insert(name.to_string(), hist.clone());
        hist
    }

    pub fn add_alert_rule(&mut self, rule: AlertRule) {
        self.alert_rules.push(rule);
    }

    /// 评估阈值告警并记录 `last_seen`
    pub fn check_alerts(&self, metric_name: &str, value: f64) {
        // 先记录最后出现时间，再评估阈值（顺序：先更新状态再评估，避免同一 tick 内 stale）
        self.last_seen
            .write()
            .insert(metric_name.to_string(), Instant::now());

        for rule in &self.alert_rules {
            if let Some(event) = rule.check(metric_name, value) {
                self.alert_events.write().push(event);
            }
        }
    }

    /// 评估缺失告警：基于 `Missing` 规则 + `last_seen` 判定指标是否超时
    pub fn check_missing_alerts(&self) -> Vec<AlertEvent> {
        let now = Instant::now();
        let last_seen = self.last_seen.read();
        let mut fired = Vec::new();
        for rule in &self.alert_rules {
            if let AlertRule::Missing { metric_name, .. } = rule {
                let last = last_seen.get(metric_name).copied();
                if let Some(event) = rule.check_missing(metric_name, last, now) {
                    self.alert_events.write().push(event.clone());
                    fired.push(event);
                }
            }
        }
        fired
    }

    pub fn get_alerts(&self) -> Vec<AlertEvent> {
        self.alert_events.read().clone()
    }

    /// 获取指定指标的最后出现时间（用于测试与外部调度器注入）
    pub fn last_seen(&self, metric_name: &str) -> Option<Instant> {
        self.last_seen.read().get(metric_name).copied()
    }

    pub fn counter(&self, name: &str) -> Option<Arc<AtomicCounter>> {
        self.counters.get(name).cloned()
    }

    pub fn gauge(&self, name: &str) -> Option<Arc<AtomicGauge>> {
        self.gauges.get(name).cloned()
    }

    pub fn histogram(&self, name: &str) -> Option<Arc<LatencyHistogram>> {
        self.histograms.get(name).cloned()
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alert::{AlertSeverity, ThresholdCondition};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_registry_counters() {
        let mut registry = MetricsRegistry::new();
        let counter = registry.register_counter("orders_total");
        counter.inc();
        counter.inc_by(5);
        assert_eq!(counter.get(), 6);
    }

    #[test]
    fn test_registry_alerts() {
        let mut registry = MetricsRegistry::new();
        registry.add_alert_rule(AlertRule::Threshold {
            metric_name: "latency_ns".into(),
            condition: ThresholdCondition::GreaterThan(10_000_000.0),
            severity: AlertSeverity::Warning,
            message: "latency exceeds 10ms".into(),
        });

        registry.check_alerts("latency_ns", 50_000_000.0);
        let alerts = registry.get_alerts();
        assert_eq!(alerts.len(), 1);
    }

    #[test]
    fn test_concurrent_counter_inc() {
        let mut registry = MetricsRegistry::new();
        let counter = registry.register_counter("orders_total");

        let mut handles = vec![];
        for _ in 0..10 {
            let counter = counter.clone();
            handles.push(std::thread::spawn(move || {
                counter.inc();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(counter.get(), 10);
    }

    // ===== Missing 规则端到端测试 =====

    #[test]
    fn test_check_alerts_records_last_seen() {
        let mut registry = MetricsRegistry::new();
        registry.register_gauge("queue_depth");
        assert!(registry.last_seen("queue_depth").is_none());
        registry.check_alerts("queue_depth", 5.0);
        assert!(registry.last_seen("queue_depth").is_some());
    }

    #[test]
    fn test_check_missing_alerts_fires_when_never_seen() {
        let mut registry = MetricsRegistry::new();
        registry.add_alert_rule(AlertRule::Missing {
            metric_name: "heartbeat".into(),
            timeout_secs: 60,
            severity: AlertSeverity::Critical,
        });
        let fired = registry.check_missing_alerts();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].severity, AlertSeverity::Critical);
    }

    #[test]
    fn test_check_missing_alerts_no_fire_after_recent_report() {
        let mut registry = MetricsRegistry::new();
        registry.register_counter("heartbeat");
        registry.add_alert_rule(AlertRule::Missing {
            metric_name: "heartbeat".into(),
            timeout_secs: 60,
            severity: AlertSeverity::Critical,
        });
        registry.check_alerts("heartbeat", 1.0);
        // 立即检查：刚刚上报，不应触发
        let fired = registry.check_missing_alerts();
        assert!(fired.is_empty());
    }

    #[test]
    fn test_check_missing_alerts_fires_after_timeout() {
        let mut registry = MetricsRegistry::new();
        registry.register_counter("heartbeat");
        registry.add_alert_rule(AlertRule::Missing {
            metric_name: "heartbeat".into(),
            timeout_secs: 0, // 任何非零间隔都视为超时
            severity: AlertSeverity::Warning,
        });
        registry.check_alerts("heartbeat", 1.0);
        // sleep 1ms 让 last_seen 严格早于 now
        thread::sleep(Duration::from_millis(2));
        let fired = registry.check_missing_alerts();
        assert_eq!(fired.len(), 1);
    }
}
