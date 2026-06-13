use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::alert::{AlertEvent, AlertRule};
use crate::metrics::{AtomicCounter, AtomicGauge, LatencyHistogram};

pub struct MetricsRegistry {
    counters: HashMap<String, Arc<AtomicCounter>>,
    gauges: HashMap<String, Arc<AtomicGauge>>,
    histograms: HashMap<String, Arc<LatencyHistogram>>,
    alert_rules: Vec<AlertRule>,
    alert_events: RwLock<Vec<AlertEvent>>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self {
            counters: HashMap::new(),
            gauges: HashMap::new(),
            histograms: HashMap::new(),
            alert_rules: Vec::new(),
            alert_events: RwLock::new(Vec::new()),
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

    pub fn check_alerts(&self, metric_name: &str, value: f64) {
        for rule in &self.alert_rules {
            if let Some(event) = rule.check(metric_name, value) {
                self.alert_events.write().push(event);
            }
        }
    }

    pub fn get_alerts(&self) -> Vec<AlertEvent> {
        self.alert_events.read().clone()
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
}
