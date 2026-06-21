use std::sync::atomic::{AtomicU64, Ordering};

pub struct AtomicCounter {
    value: AtomicU64,
}

impl AtomicCounter {
    pub fn new() -> Self {
        Self {
            value: AtomicU64::new(0),
        }
    }

    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_by(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.value.store(0, Ordering::Relaxed);
    }
}

impl Default for AtomicCounter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AtomicGauge {
    value: AtomicU64,
}

impl AtomicGauge {
    pub fn new() -> Self {
        Self {
            value: AtomicU64::new(0.0f64.to_bits()),
        }
    }

    pub fn set(&self, v: f64) {
        self.value.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f64 {
        f64::from_bits(self.value.load(Ordering::Relaxed))
    }

    pub fn add(&self, delta: f64) {
        loop {
            let current = self.value.load(Ordering::Relaxed);
            let new_val = f64::from_bits(current) + delta;
            match self.value.compare_exchange_weak(
                current,
                new_val.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(_) => continue,
            }
        }
    }
}

impl Default for AtomicGauge {
    fn default() -> Self {
        Self::new()
    }
}

pub struct LatencyHistogram {
    buckets: Vec<f64>,
    counts: Vec<AtomicU64>,
    total_count: AtomicU64,
    total_sum: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct LatencyPercentiles {
    pub p50: f64,
    pub p99: f64,
    pub p999: f64,
}

impl LatencyHistogram {
    pub fn default_latency() -> Self {
        let buckets = vec![
            10_000.0,
            50_000.0,
            100_000.0,
            500_000.0,
            1_000_000.0,
            5_000_000.0,
            10_000_000.0,
            50_000_000.0,
            100_000_000.0,
            500_000_000.0,
            1_000_000_000.0,
        ];
        let counts = (0..buckets.len()).map(|_| AtomicU64::new(0)).collect();
        Self {
            buckets,
            counts,
            total_count: AtomicU64::new(0),
            total_sum: AtomicU64::new(0.0f64.to_bits()),
        }
    }

    pub fn observe(&self, value_ns: f64) {
        self.total_count.fetch_add(1, Ordering::Relaxed);
        self.add_to_sum(value_ns);
        for (i, &bucket) in self.buckets.iter().enumerate() {
            if value_ns <= bucket {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn add_to_sum(&self, value: f64) {
        loop {
            let current = self.total_sum.load(Ordering::Relaxed);
            let new_val = f64::from_bits(current) + value;
            match self.total_sum.compare_exchange_weak(
                current,
                new_val.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(_) => continue,
            }
        }
    }

    pub fn quantile(&self, q: f64) -> f64 {
        let total = self.total_count.load(Ordering::Relaxed) as f64;
        if total == 0.0 {
            return 0.0;
        }
        let target = (q * total) as u64;
        let mut cumulative = 0u64;
        for (i, &bucket) in self.buckets.iter().enumerate() {
            cumulative += self.counts[i].load(Ordering::Relaxed);
            if cumulative >= target {
                return bucket;
            }
        }
        *self.buckets.last().unwrap_or(&0.0)
    }

    pub fn latency_percentiles(&self) -> LatencyPercentiles {
        LatencyPercentiles {
            p50: self.quantile(0.50),
            p99: self.quantile(0.99),
            p999: self.quantile(0.999),
        }
    }

    pub fn total_count(&self) -> u64 {
        self.total_count.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter() {
        let counter = AtomicCounter::new();
        counter.inc();
        counter.inc_by(5);
        assert_eq!(counter.get(), 6);
        counter.reset();
        assert_eq!(counter.get(), 0);
    }

    #[test]
    fn test_counter_default() {
        let counter = AtomicCounter::default();
        assert_eq!(counter.get(), 0);
    }

    #[test]
    fn test_counter_multiple_inc() {
        let counter = AtomicCounter::new();
        for _ in 0..100 {
            counter.inc();
        }
        assert_eq!(counter.get(), 100);
    }

    #[test]
    fn test_gauge() {
        let gauge = AtomicGauge::new();
        gauge.set(42.0);
        assert_eq!(gauge.get(), 42.0);
        gauge.add(8.0);
        assert_eq!(gauge.get(), 50.0);
    }

    #[test]
    fn test_gauge_negative() {
        let gauge = AtomicGauge::new();
        gauge.set(-100.0);
        assert_eq!(gauge.get(), -100.0);
        gauge.add(50.0);
        assert_eq!(gauge.get(), -50.0);
    }

    #[test]
    fn test_gauge_default() {
        let gauge = AtomicGauge::default();
        assert_eq!(gauge.get(), 0.0);
    }

    #[test]
    fn test_histogram() {
        let hist = LatencyHistogram::default_latency();
        hist.observe(150_000.0); // 150us
        hist.observe(500_000.0); // 500us
        hist.observe(5_000_000.0); // 5ms
        assert_eq!(hist.total_count(), 3);

        let p = hist.latency_percentiles();
        assert!(p.p50 > 0.0);
        assert!(p.p99 > 0.0);
    }

    #[test]
    fn test_histogram_quantiles() {
        let hist = LatencyHistogram::default_latency();
        // 添加 100 个样本
        for i in 1..=100 {
            hist.observe(i as f64 * 1000.0);
        }
        assert_eq!(hist.total_count(), 100);

        let p = hist.latency_percentiles();
        assert!(p.p50 > 0.0);
        assert!(p.p99 > 0.0);
        assert!(p.p999 > 0.0);
        // p99 应该大于 p50
        assert!(p.p99 >= p.p50);
    }

    #[test]
    fn test_histogram_empty() {
        let hist = LatencyHistogram::default_latency();
        assert_eq!(hist.total_count(), 0);
        let p = hist.latency_percentiles();
        assert_eq!(p.p50, 0.0);
    }
}
