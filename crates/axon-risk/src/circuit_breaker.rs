use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

pub struct CircuitBreaker {
    active: AtomicBool,
    activated_at: AtomicI64,
    daily_loss_limit: f64,
    cooldown: Duration,
}

impl CircuitBreaker {
    pub fn new(daily_loss_limit: f64, cooldown: Duration) -> Self {
        Self {
            active: AtomicBool::new(false),
            activated_at: AtomicI64::new(0),
            daily_loss_limit,
            cooldown,
        }
    }

    pub fn is_active(&self) -> bool {
        if !self.active.load(Ordering::Acquire) {
            return false;
        }
        let now = now_unix_secs();
        let activated = self.activated_at.load(Ordering::Relaxed);
        if now - activated > self.cooldown.as_secs() as i64 {
            self.active.store(false, Ordering::Release);
            return false;
        }
        true
    }

    pub fn check_and_trigger(&self, daily_pnl: f64) {
        if daily_pnl <= -self.daily_loss_limit && !self.active.load(Ordering::Relaxed) {
            self.active.store(true, Ordering::Release);
            self.activated_at.store(now_unix_secs(), Ordering::Relaxed);
        }
    }

    pub fn reset(&self) {
        self.active.store(false, Ordering::Release);
        self.activated_at.store(0, Ordering::Relaxed);
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
    fn test_circuit_breaker_initially_inactive() {
        let cb = CircuitBreaker::new(10_000.0, Duration::from_secs(60));
        assert!(!cb.is_active());
    }

    #[test]
    fn test_circuit_breaker_triggers_on_limit() {
        let cb = CircuitBreaker::new(10_000.0, Duration::from_secs(60));
        cb.check_and_trigger(-10_000.0);
        assert!(cb.is_active());
    }

    #[test]
    fn test_circuit_breaker_does_not_trigger_within_limit() {
        let cb = CircuitBreaker::new(10_000.0, Duration::from_secs(60));
        cb.check_and_trigger(-9_999.0);
        assert!(!cb.is_active());
    }

    #[test]
    fn test_circuit_breaker_reset() {
        let cb = CircuitBreaker::new(10_000.0, Duration::from_secs(60));
        cb.check_and_trigger(-10_000.0);
        assert!(cb.is_active());
        cb.reset();
        assert!(!cb.is_active());
    }

    #[test]
    fn test_circuit_breaker_positive_pnl_no_trigger() {
        let cb = CircuitBreaker::new(10_000.0, Duration::from_secs(60));
        cb.check_and_trigger(5_000.0);
        assert!(!cb.is_active());
    }
}
