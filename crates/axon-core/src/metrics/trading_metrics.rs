//! 交易胜率/夏普等指标收集器
//!
//! 线程安全(AtomicI64),用 i64 × 1e6 定点数记录盈亏/手续费,避免浮点竞态。

use std::sync::atomic::{AtomicI64, Ordering};

/// 交易指标收集器(线程安全)
#[derive(Debug, Default)]
pub struct TradingMetrics {
    /// 盈利交易数(pnl > 0)
    wins: AtomicI64,
    /// 亏损交易数(pnl < 0)
    losses: AtomicI64,
    /// 总盈亏(× 1e6,定点数)
    total_pnl: AtomicI64,
    /// 总手续费(× 1e6,定点数)
    total_fees: AtomicI64,
    /// 累计 log return(× 1e9,定点数)
    log_return_sum: AtomicI64,
    /// 累计 log return 平方(× 1e18,定点数)
    log_return_sq_sum: AtomicI64,
    /// log return 计数(用于 sharpe 计算分母)
    log_return_count: AtomicI64,
    /// 交易笔数
    trade_count: AtomicI64,
}

impl TradingMetrics {
    /// 新建
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录单笔交易(以 i64 × 1e6 定点数传入,避免浮点竞态)
    pub fn record_trade(&self, pnl: i64, fees: i64) {
        if pnl > 0 {
            self.wins.fetch_add(1, Ordering::Relaxed);
        } else if pnl < 0 {
            self.losses.fetch_add(1, Ordering::Relaxed);
        }
        self.total_pnl.fetch_add(pnl, Ordering::Relaxed);
        self.total_fees.fetch_add(fees, Ordering::Relaxed);
        self.trade_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录单步 log return(× 1e9 定点)
    pub fn record_log_return(&self, lr: i64) {
        self.log_return_sum.fetch_add(lr, Ordering::Relaxed);
        // sq 用 × 1e18 定点存储(lr² 自然在 1e18 量级,saturating_mul 防溢出)
        self.log_return_sq_sum
            .fetch_add(lr.saturating_mul(lr), Ordering::Relaxed);
        self.log_return_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 胜率
    pub fn win_rate(&self) -> f64 {
        let n = self.trade_count.load(Ordering::Relaxed);
        if n == 0 {
            return 0.0;
        }
        self.wins.load(Ordering::Relaxed) as f64 / n as f64
    }

    /// 夏普比率(基于 log return 年化)
    ///
    /// `sqrt(periods_per_year) * mean(log_return) / std(log_return)`
    pub fn sharpe_ratio(&self, periods_per_year: f64) -> f64 {
        let n = self.log_return_count.load(Ordering::Relaxed);
        if n < 2 {
            return 0.0;
        }
        let n_f = n as f64;
        let mean = self.log_return_sum.load(Ordering::Relaxed) as f64 / 1e9 / n_f;
        // sum_sq 单位是 1e18,所以 sum_sq / 1e18 / n = E[lr²]
        let var = (self.log_return_sq_sum.load(Ordering::Relaxed) as f64 / 1e18 / n_f) - mean * mean;
        if var <= 0.0 {
            return 0.0;
        }
        mean / var.sqrt() * periods_per_year.sqrt()
    }

    /// 总盈亏(f64)
    pub fn total_pnl_f64(&self) -> f64 {
        self.total_pnl.load(Ordering::Relaxed) as f64 / 1e6
    }

    /// 总手续费(f64)
    pub fn total_fees_f64(&self) -> f64 {
        self.total_fees.load(Ordering::Relaxed) as f64 / 1e6
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_win_rate_basic() {
        let m = TradingMetrics::new();
        m.record_trade(100_000, 50_000); // win
        m.record_trade(-50_000, 50_000); // loss
        m.record_trade(200_000, 50_000); // win
        assert_eq!(m.trade_count.load(Ordering::Relaxed), 3);
        assert_eq!(m.wins.load(Ordering::Relaxed), 2);
        assert_eq!(m.losses.load(Ordering::Relaxed), 1);
        assert!((m.win_rate() - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn test_sharpe_zero_variance() {
        let m = TradingMetrics::new();
        // 所有 log return 相同 → 方差为 0 → sharpe = 0
        for _ in 0..10 {
            m.record_log_return(100_000_000); // 0.1
        }
        assert_eq!(m.sharpe_ratio(252.0), 0.0);
    }

    #[test]
    fn test_sharpe_positive() {
        let m = TradingMetrics::new();
        // log_return: [0.1, 0.2, 0.15, 0.18, 0.12]
        for v in [0.1, 0.2, 0.15, 0.18, 0.12] {
            m.record_log_return((v * 1e9) as i64);
        }
        // mean ≈ 0.15, std 略 > 0
        let s = m.sharpe_ratio(252.0);
        let n = m.trade_count.load(std::sync::atomic::Ordering::Relaxed);
        let sum_lr = m.log_return_sum.load(std::sync::atomic::Ordering::Relaxed);
        let sum_sq = m.log_return_sq_sum.load(std::sync::atomic::Ordering::Relaxed);
        let mean = sum_lr as f64 / 1e9 / n as f64;
        let e_lr2 = sum_sq as f64 / 1e18 / n as f64;
        let var = e_lr2 - mean * mean;
        assert!(
            s > 0.0,
            "正收益的 sharpe 应该 > 0,实际 {} (n={} sum_lr={} sum_sq={} mean={} e_lr2={} var={})",
            s, n, sum_lr, sum_sq, mean, e_lr2, var
        );
    }

    #[test]
    fn test_total_pnl_and_fees() {
        let m = TradingMetrics::new();
        m.record_trade(1_000_000, 100_000);
        m.record_trade(-500_000, 100_000);
        assert!((m.total_pnl_f64() - 0.5).abs() < 1e-6);
        assert!((m.total_fees_f64() - 0.2).abs() < 1e-6);
    }
}
