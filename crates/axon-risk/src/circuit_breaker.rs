use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use crate::utils::now_unix_secs;

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

    /// 热更日亏熔断阈值：仅影响下一次 check_and_trigger 判定，
    /// 不追溯激活态（已触发的熔断不由本方法解除）。
    pub fn update_daily_loss_limit(&mut self, limit: f64) {
        self.daily_loss_limit = limit;
    }

    /// 热更熔断冷却时长：已激活熔断的 activated_at 时间戳不变，
    /// 只改变自恢复等待窗口。
    pub fn update_cooldown(&mut self, cooldown: Duration) {
        self.cooldown = cooldown;
    }
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

    // ===== 热更（DefaultRiskEngine::set_config 配套，spec §3.2）=====

    #[test]
    fn update_daily_loss_limit_takes_effect_immediately() {
        let mut cb = CircuitBreaker::new(10_000.0, Duration::from_secs(60));
        cb.check_and_trigger(-5_000.0); // 未达原阈值
        assert!(!cb.is_active());
        cb.update_daily_loss_limit(1_000.0);
        // check_and_trigger 收累计亏损值：热更后下一次回灌立即按新阈值判定
        cb.check_and_trigger(-5_001.0);
        assert!(cb.is_active());
    }

    #[test]
    fn update_cooldown_shortens_recovery() {
        let mut cb = CircuitBreaker::new(1_000.0, Duration::from_secs(3600));
        cb.check_and_trigger(-1_000.0);
        assert!(cb.is_active()); // 冷却窗口内
        // activated_at 不变，仅冷却归零 → 下一秒起自恢复
        cb.update_cooldown(Duration::from_secs(0));
        std::thread::sleep(Duration::from_millis(1100));
        assert!(!cb.is_active());
    }
}
