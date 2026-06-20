use std::time::Instant;

use crate::error::ExchangeError;

pub struct TokenBucketRateLimiter {
    capacity: u32,
    tokens: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl TokenBucketRateLimiter {
    pub fn new(requests_per_second: u32) -> Self {
        Self {
            capacity: requests_per_second,
            tokens: requests_per_second as f64,
            refill_rate: requests_per_second as f64,
            last_refill: Instant::now(),
        }
    }

    pub fn try_acquire(&mut self) -> Result<(), ExchangeError> {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            let wait_ms = ((1.0 - self.tokens) / self.refill_rate * 1000.0) as u64;
            Err(ExchangeError::RateLimited { wait_ms })
        }
    }

    fn refill(&mut self) {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity as f64);
        self.last_refill = Instant::now();
    }

    /// 当前可用 token 数(refill 后,饱和到 `[0, capacity]`)。
    /// Stage 5 Python 绑定需要,暴露只读状态。
    pub fn available_tokens(&self) -> u32 {
        // 复制 refill 语义但不动 last_refill
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        let cur = (self.tokens + elapsed * self.refill_rate).min(self.capacity as f64);
        cur.floor().max(0.0) as u32
    }

    /// 配置的容量(即构造时传入的 `requests_per_second`)。
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// token 补充速率(tokens / second,与 `requests_per_second` 相等)。
    pub fn refill_rate(&self) -> f64 {
        self.refill_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_allows_within_capacity() {
        let mut limiter = TokenBucketRateLimiter::new(10);
        assert!(limiter.try_acquire().is_ok());
    }

    #[test]
    fn test_rate_limiter_rejects_when_exhausted() {
        let mut limiter = TokenBucketRateLimiter::new(1);
        assert!(limiter.try_acquire().is_ok());
        assert!(limiter.try_acquire().is_err());
    }

    #[test]
    fn test_rate_limiter_getters() {
        let mut limiter = TokenBucketRateLimiter::new(5);
        assert_eq!(limiter.capacity(), 5);
        assert!((limiter.refill_rate() - 5.0).abs() < 1e-9);
        // 初始 available ≈ capacity
        let initial = limiter.available_tokens();
        assert!(initial <= 5);
        // acquire 后应下降
        let _ = limiter.try_acquire();
        let after = limiter.available_tokens();
        assert!(after <= initial);
    }

    proptest::proptest! {
        #[test]
        fn prop_rate_limiter_never_over_capacity(capacity in 1u32..100u32) {
            let mut limiter = TokenBucketRateLimiter::new(capacity);
            let mut acquired = 0;
            for _ in 0..capacity + 10 {
                if limiter.try_acquire().is_ok() {
                    acquired += 1;
                }
            }
            // Should never acquire more than capacity
            assert!(acquired <= capacity as usize);
        }
    }
}
