//! 熔断器
//!
//! 使用 `AtomicU8` 存储状态，`check()` 热路径 < 20ns。
//! 只在 `record_trade()` / `record_failure()` 时加锁（低频操作）。

use std::sync::atomic::{AtomicU8, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

const STATE_CLOSED: u8 = 0;
const STATE_OPEN: u8 = 1;
const STATE_HALF_OPEN: u8 = 2;

/// 熔断器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    /// 最大连续失败次数，默认 3
    pub max_consecutive_failures: u64,
    /// 冷却时间（秒），默认 60
    pub cooldown_seconds: u64,
    /// 最大日亏损百分比，默认 2.0 (2%)
    pub max_daily_loss_pct: f64,
    /// 单品种最大仓位百分比，默认 20.0 (20%)
    pub max_position_pct: f64,
    /// 最大日交易次数，默认 100
    pub max_daily_trades: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 3,
            cooldown_seconds: 60,
            max_daily_loss_pct: 2.0,
            max_position_pct: 20.0,
            max_daily_trades: 100,
        }
    }
}

/// 熔断器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BreakerState {
    /// 正常运行
    Closed,
    /// 熔断中，拒绝所有请求
    Open,
    /// 冷却期，允许少量试探请求
    HalfOpen,
}

/// 内部共享状态
struct Inner {
    config: CircuitBreakerConfig,
    consecutive_failures: parking_lot::Mutex<u64>,
    daily_loss: parking_lot::Mutex<f64>,
    daily_trades: parking_lot::Mutex<u64>,
    last_trade_day: parking_lot::Mutex<u64>,
    open_since: parking_lot::Mutex<u64>,
}

/// 生产级熔断器
///
/// - `check()` 热路径：单次原子加载，延迟 < 20ns
/// - `record_trade()` 低频操作：加锁更新统计，检查是否触发熔断
pub struct CircuitBreaker {
    state: AtomicU8,
    inner: Inner,
}

impl CircuitBreaker {
    /// 创建熔断器
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: AtomicU8::new(STATE_CLOSED),
            inner: Inner {
                config,
                consecutive_failures: Mutex::new(0),
                daily_loss: Mutex::new(0.0),
                daily_trades: Mutex::new(0),
                last_trade_day: Mutex::new(0),
                open_since: Mutex::new(0),
            },
        }
    }

    /// 热路径检查：是否允许交易
    ///
    /// 延迟 < 20ns，仅一次原子加载。
    #[inline(always)]
    pub fn check(&self) -> bool {
        self.state.load(Ordering::Relaxed) == STATE_CLOSED
    }

    /// 当前状态
    #[inline]
    pub fn state(&self) -> BreakerState {
        match self.state.load(Ordering::Relaxed) {
            STATE_CLOSED => BreakerState::Closed,
            STATE_OPEN => BreakerState::Open,
            STATE_HALF_OPEN => BreakerState::HalfOpen,
            _ => BreakerState::Open,
        }
    }

    /// 是否处于熔断状态
    #[inline]
    pub fn is_open(&self) -> bool {
        self.state.load(Ordering::Relaxed) == STATE_OPEN
    }

    /// 记录交易结果
    ///
    /// 检查是否触发熔断条件：
    /// 1. 连续失败次数 ≥ 阈值
    /// 2. 日亏损 > 阈值
    /// 3. 仓位超限
    /// 4. 日交易次数 ≥ 阈值
    pub fn record_trade(&self, pnl: f64, _symbol: &str, position_pct: f64) {
        let today = Self::today_secs();

        // 日切逻辑
        {
            let mut last = self.inner.last_trade_day.lock();
            if *last != today {
                *last = today;
                *self.inner.daily_loss.lock() = 0.0;
                *self.inner.daily_trades.lock() = 0;
            }
        }

        // 如果已熔断，检查冷却
        if self.state.load(Ordering::Relaxed) == STATE_OPEN {
            let open_since = *self.inner.open_since.lock();
            let now = Self::now_secs();
            if now - open_since >= self.inner.config.cooldown_seconds {
                self.state.store(STATE_HALF_OPEN, Ordering::Relaxed);
            }
            return;
        }

        // 更新统计
        let consecutive = {
            let mut failures = self.inner.consecutive_failures.lock();
            if pnl < 0.0 {
                *failures += 1;
            } else {
                *failures = 0;
            }
            *failures
        };

        {
            let mut loss = self.inner.daily_loss.lock();
            if pnl < 0.0 {
                *loss += pnl.abs();
            }
        }

        {
            let mut trades = self.inner.daily_trades.lock();
            *trades += 1;
        }

        // 检查熔断条件
        if consecutive >= self.inner.config.max_consecutive_failures {
            self.trip("连续失败");
            return;
        }

        let daily_loss = *self.inner.daily_loss.lock();
        if daily_loss > self.inner.config.max_daily_loss_pct {
            self.trip("日亏损超限");
            return;
        }

        if position_pct > self.inner.config.max_position_pct {
            self.trip("仓位超限");
            return;
        }

        let trades = *self.inner.daily_trades.lock();
        if trades >= self.inner.config.max_daily_trades {
            self.trip("日交易次数超限");
        }
    }

    /// 记录失败（无交易，如 API 错误）
    pub fn record_failure(&self, _reason: &str) {
        let mut failures = self.inner.consecutive_failures.lock();
        *failures += 1;
        if *failures >= self.inner.config.max_consecutive_failures {
            drop(failures);
            self.trip("连续失败");
        }
    }

    /// 强制重置（人工干预）
    pub fn force_reset(&self) {
        self.state.store(STATE_CLOSED, Ordering::Relaxed);
        *self.inner.consecutive_failures.lock() = 0;
        *self.inner.daily_loss.lock() = 0.0;
        *self.inner.daily_trades.lock() = 0;
    }

    fn trip(&self, _reason: &str) {
        self.state.store(STATE_OPEN, Ordering::Relaxed);
        *self.inner.open_since.lock() = Self::now_secs();
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn today_secs() -> u64 {
        // 简化：用天级精度（每天重置一次）
        Self::now_secs() / 86400
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            max_consecutive_failures: 3,
            cooldown_seconds: 0, // 测试用 0 秒冷却
            max_daily_loss_pct: 5.0,
            max_position_pct: 20.0,
            max_daily_trades: 100,
        }
    }

    #[test]
    fn test_basic_check() {
        let cb = CircuitBreaker::new(test_config());
        assert!(cb.check());
        assert_eq!(cb.state(), BreakerState::Closed);
        assert!(!cb.is_open());
    }

    #[test]
    fn test_consecutive_failures_trip() {
        let cb = CircuitBreaker::new(test_config());
        cb.record_trade(-1.0, "BTC", 10.0);
        cb.record_trade(-1.0, "BTC", 10.0);
        assert!(cb.check()); // 2 次，未触发
        cb.record_trade(-1.0, "BTC", 10.0);
        assert!(!cb.check()); // 3 次，触发熔断
        assert!(cb.is_open());
    }

    #[test]
    fn test_daily_loss_trip() {
        let config = CircuitBreakerConfig {
            max_consecutive_failures: 100,
            cooldown_seconds: 0,
            max_daily_loss_pct: 5.0,
            max_position_pct: 20.0,
            max_daily_trades: 100,
        };
        let cb = CircuitBreaker::new(config);
        cb.record_trade(-2.0, "BTC", 10.0);
        cb.record_trade(-2.0, "ETH", 10.0);
        assert!(cb.check()); // 4.0 < 5.0
        cb.record_trade(-1.5, "SOL", 10.0);
        assert!(!cb.check()); // 5.5 > 5.0
    }

    #[test]
    fn test_position_limit_trip() {
        let config = CircuitBreakerConfig {
            max_consecutive_failures: 100,
            cooldown_seconds: 0,
            max_daily_loss_pct: 100.0,
            max_position_pct: 20.0,
            max_daily_trades: 100,
        };
        let cb = CircuitBreaker::new(config);
        cb.record_trade(1.0, "BTC", 15.0);
        assert!(cb.check());
        cb.record_trade(1.0, "BTC", 25.0);
        assert!(!cb.check()); // 25% > 20%
    }

    #[test]
    fn test_cooldown_recovery() {
        let config = CircuitBreakerConfig {
            max_consecutive_failures: 2,
            cooldown_seconds: 0,
            max_daily_loss_pct: 100.0,
            max_position_pct: 100.0,
            max_daily_trades: 100,
        };
        let cb = CircuitBreaker::new(config);
        cb.record_trade(-1.0, "BTC", 10.0);
        cb.record_trade(-1.0, "BTC", 10.0);
        assert!(cb.is_open());
        // 冷却时间为 0，下次 record_trade 应转为 HALF_OPEN
        cb.record_trade(1.0, "BTC", 10.0);
        assert_eq!(cb.state(), BreakerState::HalfOpen);
    }

    #[test]
    fn test_force_reset() {
        let cb = CircuitBreaker::new(test_config());
        cb.record_trade(-1.0, "BTC", 10.0);
        cb.record_trade(-1.0, "BTC", 10.0);
        cb.record_trade(-1.0, "BTC", 10.0);
        assert!(cb.is_open());
        cb.force_reset();
        assert!(cb.check());
        assert_eq!(cb.state(), BreakerState::Closed);
    }
}
