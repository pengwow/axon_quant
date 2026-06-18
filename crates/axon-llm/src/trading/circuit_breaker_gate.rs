//! Circuit breaker gate 适配
//!
//! 提供两个 `RiskGate` 实现,均为 `PlaceOrderTool` 真发订单前的最后闸门:
//!
//! - [`RejectionCircuitBreaker`]:核心 lib,基于"连续 N 次风控拒绝"开闸,
//!   **零新增依赖**(只依赖 `std::sync::atomic`)。LLM agent 场景下使用,
//!   防止 LLM 重复触发同类违规订单。
//! - [`RiskPnLCircuitBreaker`]:feature-gated(`trading-risk-extra`),
//!   包装 `axon_risk::circuit_breaker::CircuitBreaker`,基于日 PnL 触发。
//!   实盘 / testnet 场景下使用,日亏损达到上限时自动暂停下单。
//!
//! **设计动机**:`axon-llm` lib 默认零传递依赖,所有 `axon-risk` 类型
//! 通过 feature 隔离;具体桥接 adapter(本模块)由 lib 提供,使用方
//! 可直接 `use`,无需在 demo / 业务 crate 重复实现。

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::time::Duration;

use crate::trading::safety::RiskGate;

// ── RejectionCircuitBreaker(核心 lib,零新增依赖)────────────────

/// 基于"连续 N 次风控拒绝"开闸的 circuit breaker
///
/// **线程安全**:所有状态用 `AtomicU32` / `AtomicI64` + `Ordering` 保护,
/// 无锁并发访问(LLM agent 多线程并发时不会出现状态错乱)。
///
/// **冷却机制**:`is_blocked()` 调用时,如果当前时间 - 开闸时间 > cooldown,
/// 自动重置(自愈,无需后台任务)。
///
/// **触发位置**(由 `PlaceOrderTool` 调):
/// - `record_rejection()`:`RiskLimits::check` 失败时(白名单 / 单笔金额 /
///   max_position_abs 全部算)
/// - `record_success()`:真发订单成功后(`DryRun` / `Direct` / `TwoPhase` 全部算)
///
/// **不触发场景**:后端错误(网络超时 / 拒单) — 这些不是"风控恢复"信号,
/// 保持计数避免被错误地清零。
pub struct RejectionCircuitBreaker {
    /// 连续拒绝次数(开闸时 / record_success 时清零)
    rejection_count: AtomicU32,
    /// 连续拒绝阈值(N 次后开闸)
    threshold: u32,
    /// 触发开闸的时间(unix_secs;0 表示未开闸)
    activated_at: AtomicI64,
    /// 开闸持续时间
    cooldown: Duration,
}

impl RejectionCircuitBreaker {
    /// 构造新闸门
    ///
    /// - `threshold`:连续 N 次拒绝后开闸,推荐 3~5
    /// - `cooldown`:开闸后持续时间,推荐 30s~5min
    pub fn new(threshold: u32, cooldown: Duration) -> Self {
        Self {
            rejection_count: AtomicU32::new(0),
            threshold,
            activated_at: AtomicI64::new(0),
            cooldown,
        }
    }

    /// 记录一次风控拒绝(由 `PlaceOrderTool` 在 `RiskLimits::check` 失败时调用)
    ///
    /// 达到 `threshold` 时自动开闸;开闸后继续累加计数(等 cooldown 自愈)。
    pub fn record_rejection(&self) {
        let count = self.rejection_count.fetch_add(1, Ordering::AcqRel) + 1;
        if count >= self.threshold {
            // compare-and-swap:防止多次开闸时 activated_at 倒退
            // (0 → now,失败说明已被其他线程设为非 0,保持原值)
            let _ = self.activated_at.compare_exchange(
                0,
                now_unix_ms(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            );
        }
    }

    /// 记录一次成功(下单成功)→ 清零拒绝计数
    ///
    /// **触发位置**:`PlaceOrderTool` 真发订单成功后(后端返回 `OrderAck`,
    /// 风控未阻断)。**不用于后端错误**(后端错误不计为"风控恢复")。
    pub fn record_success(&self) {
        self.rejection_count.store(0, Ordering::Release);
    }

    /// 查询当前开闸状态(不重置,纯读)
    ///
    /// **与 `RiskGate::is_blocked` 的区别**:本方法不触发 cooldown 自愈
    /// 副作用,适合做状态查询 / 监控埋点。
    pub fn is_active(&self) -> bool {
        let activated = self.activated_at.load(Ordering::Acquire);
        if activated == 0 {
            return false;
        }
        let now = now_unix_ms();
        now - activated <= self.cooldown.as_millis() as i64
    }

    /// 当前连续拒绝计数(只读,监控 / 测试用)
    pub fn rejection_count(&self) -> u32 {
        self.rejection_count.load(Ordering::Relaxed)
    }
}

impl RiskGate for RejectionCircuitBreaker {
    fn is_blocked(&self) -> Option<String> {
        let activated = self.activated_at.load(Ordering::Acquire);
        if activated == 0 {
            return None; // 未开闸
        }
        let now = now_unix_ms();
        let cooldown_ms = self.cooldown.as_millis() as i64;
        if now - activated > cooldown_ms {
            // cooldown 结束,自动重置
            self.activated_at.store(0, Ordering::Release);
            self.rejection_count.store(0, Ordering::Release);
            return None;
        }
        let remaining_ms = cooldown_ms - (now - activated);
        Some(format!(
            "rejection circuit breaker active: {} consecutive rejections, cooldown {}ms remaining",
            self.rejection_count.load(Ordering::Relaxed),
            remaining_ms
        ))
    }
}

// ── RiskPnLCircuitBreaker(feature-gated)────────────────────────

/// 包装 `axon_risk::circuit_breaker::CircuitBreaker`,适配为 `RiskGate`
///
/// **PnL 触发**:`axon_risk::CircuitBreaker::check_and_trigger(daily_pnl)`
/// 当 `daily_pnl <= -daily_loss_limit` 时开闸。本适配只读 `is_active()`,
/// PnL 计算由使用方(回测 / 实盘 portfolio)驱动。
///
/// **典型用法**(实盘 / testnet demo):
/// ```ignore
/// use std::sync::Arc;
/// use std::time::Duration;
/// use axon_llm::trading::circuit_breaker_gate::RiskPnLCircuitBreaker;
/// use axon_risk::circuit_breaker::CircuitBreaker;
///
/// let cb = Arc::new(CircuitBreaker::new(10_000.0, Duration::from_secs(60)));
/// let gate: Arc<dyn RiskGate> = Arc::new(RiskPnLCircuitBreaker::new(cb.clone()));
/// // portfolio 每日结算后调 cb.check_and_trigger(daily_pnl)
/// ```
#[cfg(feature = "trading-risk-extra")]
pub struct RiskPnLCircuitBreaker {
    cb: std::sync::Arc<axon_risk::circuit_breaker::CircuitBreaker>,
}

#[cfg(feature = "trading-risk-extra")]
impl RiskPnLCircuitBreaker {
    /// 包装 `axon_risk::CircuitBreaker`
    pub fn new(cb: std::sync::Arc<axon_risk::circuit_breaker::CircuitBreaker>) -> Self {
        Self { cb }
    }
}

#[cfg(feature = "trading-risk-extra")]
impl RiskGate for RiskPnLCircuitBreaker {
    fn is_blocked(&self) -> Option<String> {
        if self.cb.is_active() {
            Some("PnL circuit breaker active (daily loss limit exceeded)".to_string())
        } else {
            None
        }
    }
}

// ── 内部辅助 ─────────────────────────────────────────────────────

/// 当前 unix_ms(取系统时间,失败回退 0)
fn now_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── 测试 ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn breaker_initially_does_not_block() {
        let b = RejectionCircuitBreaker::new(3, Duration::from_secs(60));
        assert!(b.is_blocked().is_none());
        assert!(!b.is_active());
    }

    #[test]
    fn breaker_opens_after_threshold_rejections() {
        let b = RejectionCircuitBreaker::new(3, Duration::from_secs(60));
        b.record_rejection();
        b.record_rejection();
        assert!(b.is_blocked().is_none(), "2 次拒绝未达阈值,应放行");
        b.record_rejection();
        assert!(b.is_active(), "3 次拒绝达到阈值,应开闸");
        let msg = b.is_blocked().expect("应阻断");
        assert!(
            msg.contains("rejection circuit breaker active"),
            "msg = {}",
            msg
        );
    }

    #[test]
    fn breaker_resets_count_on_success() {
        let b = RejectionCircuitBreaker::new(3, Duration::from_secs(60));
        b.record_rejection();
        b.record_rejection();
        b.record_success();
        assert_eq!(b.rejection_count(), 0);
        // 再次 reject 2 次,仍不应开闸
        b.record_rejection();
        b.record_rejection();
        assert!(b.is_blocked().is_none(), "清零后第 2 次仍不应开闸");
    }

    #[test]
    fn breaker_resets_after_cooldown() {
        let b = RejectionCircuitBreaker::new(1, Duration::from_millis(100));
        b.record_rejection();
        assert!(b.is_active());
        thread::sleep(Duration::from_millis(200));
        // cooldown 自愈
        assert!(b.is_blocked().is_none(), "cooldown 后应自愈");
        assert!(!b.is_active());
        assert_eq!(b.rejection_count(), 0, "cooldown 后计数应清零");
    }

    #[test]
    fn breaker_high_threshold_never_opens_with_few_rejections() {
        let b = RejectionCircuitBreaker::new(100, Duration::from_secs(60));
        for _ in 0..50 {
            b.record_rejection();
        }
        assert!(b.is_blocked().is_none(), "50 次拒绝未达 100 阈值,应放行");
        assert_eq!(b.rejection_count(), 50);
    }

    #[test]
    fn breaker_blocked_message_contains_cooldown_remaining() {
        let b = RejectionCircuitBreaker::new(1, Duration::from_secs(60));
        b.record_rejection();
        let msg = b.is_blocked().expect("应阻断");
        assert!(msg.contains("cooldown"), "msg 应含 cooldown: {}", msg);
        assert!(msg.contains("remaining"), "msg 应含 remaining: {}", msg);
    }

    #[test]
    fn breaker_thread_safe_under_concurrent_rejections() {
        let b = Arc::new(RejectionCircuitBreaker::new(50, Duration::from_secs(60)));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let b = b.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    b.record_rejection();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // 10 线程 * 100 = 1000 次拒绝
        assert_eq!(b.rejection_count(), 1000);
        assert!(b.is_active(), "达到 50 阈值应开闸");
    }

    #[test]
    fn breaker_is_active_and_is_blocked_consistent() {
        let b = RejectionCircuitBreaker::new(2, Duration::from_secs(60));
        assert_eq!(b.is_active(), b.is_blocked().is_some());
        b.record_rejection();
        assert_eq!(b.is_active(), b.is_blocked().is_some());
        b.record_rejection();
        assert_eq!(b.is_active(), b.is_blocked().is_some());
    }
}

#[cfg(all(test, feature = "trading-risk-extra"))]
mod pnl_breaker_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use axon_risk::circuit_breaker::CircuitBreaker;

    #[test]
    fn pnl_breaker_initially_does_not_block() {
        let cb = Arc::new(CircuitBreaker::new(10_000.0, Duration::from_secs(60)));
        let g = RiskPnLCircuitBreaker::new(cb);
        assert!(g.is_blocked().is_none());
    }

    #[test]
    fn pnl_breaker_blocks_after_pnl_triggers() {
        let cb = Arc::new(CircuitBreaker::new(10_000.0, Duration::from_secs(60)));
        cb.check_and_trigger(-10_000.0);
        let g = RiskPnLCircuitBreaker::new(cb);
        let msg = g.is_blocked().expect("应阻断");
        assert!(msg.contains("PnL circuit breaker"), "msg = {}", msg);
    }

    #[test]
    fn pnl_breaker_resets_when_cb_resets() {
        let cb = Arc::new(CircuitBreaker::new(10_000.0, Duration::from_secs(60)));
        cb.check_and_trigger(-10_000.0);
        let g = RiskPnLCircuitBreaker::new(cb.clone());
        assert!(g.is_blocked().is_some());
        cb.reset();
        assert!(g.is_blocked().is_none());
    }
}
