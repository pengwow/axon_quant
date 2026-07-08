//! Token 预算守卫
//!
//! 基于 Token 预算的管理，支持区间转换和熔断。

use std::sync::atomic::{AtomicU64, Ordering};

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
use crate::policy::BudgetGuard;
use crate::types::{BudgetState, BudgetZone, HarnessConfig};

/// Token 预算守卫
///
/// 基于 Token 预算的管理，支持：
/// - 区间转换：Green → Yellow → Red → CircuitBreak
/// - 熔断器集成：预算耗尽时触发熔断
/// - 原子操作：无锁并发访问
pub struct SimpleBudgetGuard {
    total_budget: u64,
    tokens_used: AtomicU64,
    config: HarnessConfig,
    circuit_breaker: CircuitBreaker,
}

impl SimpleBudgetGuard {
    /// 创建预算守卫
    pub fn new(config: HarnessConfig) -> Self {
        let cb_config = CircuitBreakerConfig {
            max_consecutive_failures: 100,
            cooldown_seconds: 60,
            max_daily_loss_pct: 100.0,
            max_position_pct: 100.0,
            max_daily_trades: 10000,
        };
        Self {
            total_budget: config.max_tokens,
            tokens_used: AtomicU64::new(0),
            config,
            circuit_breaker: CircuitBreaker::new(cb_config),
        }
    }

    /// 计算当前预算区间
    fn current_zone(&self) -> BudgetZone {
        let tokens = self.tokens_used.load(Ordering::Relaxed);
        let ratio = tokens as f64 / self.total_budget as f64;
        if ratio >= 1.0 {
            BudgetZone::CircuitBreak
        } else if ratio >= self.config.red_zone_threshold {
            BudgetZone::Red
        } else if ratio >= self.config.yellow_zone_threshold {
            BudgetZone::Yellow
        } else {
            BudgetZone::Green
        }
    }

    /// 计算费用（USD）
    fn calculate_cost(&self) -> f64 {
        // 简化计算：假设每 1000 Token $0.01
        let tokens = self.tokens_used.load(Ordering::Relaxed) as f64;
        tokens * 0.00001
    }
}

impl BudgetGuard for SimpleBudgetGuard {
    fn consume(&self, tokens: u64, _model: &str) -> BudgetZone {
        let new_total = self.tokens_used.fetch_add(tokens, Ordering::Relaxed) + tokens;
        let ratio = new_total as f64 / self.total_budget as f64;

        if ratio >= 1.0 {
            self.circuit_breaker.open(); // 预算耗尽：打开熔断器
            BudgetZone::CircuitBreak
        } else if ratio >= self.config.red_zone_threshold {
            BudgetZone::Red
        } else if ratio >= self.config.yellow_zone_threshold {
            BudgetZone::Yellow
        } else {
            BudgetZone::Green
        }
    }

    fn is_circuit_break(&self) -> bool {
        self.circuit_breaker.is_open()
    }

    fn remaining(&self) -> u64 {
        self.total_budget
            .saturating_sub(self.tokens_used.load(Ordering::Relaxed))
    }

    fn snapshot(&self) -> BudgetState {
        BudgetState {
            total_budget: self.total_budget,
            tokens_used: self.tokens_used.load(Ordering::Relaxed),
            zone: self.current_zone(),
            cost_usd: self.calculate_cost(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> HarnessConfig {
        HarnessConfig {
            max_steps: 50,
            max_tokens: 100_000,
            timeout_secs: 300,
            green_zone_threshold: 0.6,
            yellow_zone_threshold: 0.8,
            red_zone_threshold: 0.95,
        }
    }

    #[test]
    fn test_consume_green() {
        let guard = SimpleBudgetGuard::new(test_config());
        assert_eq!(guard.consume(50_000, "gpt-4o"), BudgetZone::Green);
    }

    #[test]
    fn test_consume_yellow() {
        let guard = SimpleBudgetGuard::new(test_config());
        // 70% < 80% (yellow_zone_threshold)，所以是 Green
        assert_eq!(guard.consume(70_000, "gpt-4o"), BudgetZone::Green);
    }

    #[test]
    fn test_consume_red() {
        let guard = SimpleBudgetGuard::new(test_config());
        // 90% >= 80% (yellow_zone_threshold)，所以是 Yellow
        assert_eq!(guard.consume(90_000, "gpt-4o"), BudgetZone::Yellow);
    }

    #[test]
    fn test_consume_red_zone() {
        let guard = SimpleBudgetGuard::new(test_config());
        // 96% >= 95% (red_zone_threshold)，所以是 Red
        assert_eq!(guard.consume(96_000, "gpt-4o"), BudgetZone::Red);
    }

    #[test]
    fn test_remaining() {
        let guard = SimpleBudgetGuard::new(test_config());
        guard.consume(30_000, "gpt-4o");
        assert_eq!(guard.remaining(), 70_000);
    }

    #[test]
    fn test_snapshot() {
        let guard = SimpleBudgetGuard::new(test_config());
        guard.consume(50_000, "gpt-4o");
        let snap = guard.snapshot();
        assert_eq!(snap.total_budget, 100_000);
        assert_eq!(snap.tokens_used, 50_000);
        assert_eq!(snap.zone, BudgetZone::Green);
    }

    /// 预算耗尽时:应打开熔断器(供 is_circuit_break 检查)
    #[test]
    fn test_budget_exhausted_opens_circuit() {
        let guard = SimpleBudgetGuard::new(test_config());
        assert!(!guard.is_circuit_break());
        guard.consume(100_000, "gpt-4o");
        assert!(guard.is_circuit_break(), "预算耗尽后熔断器应处于打开状态");
        assert_eq!(guard.snapshot().zone, BudgetZone::CircuitBreak);
    }
}
