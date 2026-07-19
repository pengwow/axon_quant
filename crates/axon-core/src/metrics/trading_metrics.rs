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
    /// # Args
    /// - `periods_per_year`:一年 bar 数(不是 sqrt!内部会自动 `sqrt()`)。
    ///   常见值:
    ///   - 15-min bar → 35_040 (`365 * 24 * 4`)
    ///   - 1h bar    → 8_760 (`365 * 24`)
    ///   - 1d bar    → 365
    ///   - 1min bar  → 525_600 (`365 * 24 * 60`)
    ///
    /// # 边界
    /// - 样本数 `< 2` → 返回 `0.0`(无统计意义)
    /// - 样本数 `< 30` → `tracing::warn!` 提示统计意义不足
    /// - 方差 `<= 0`(单调行情)→ 返回 `0.0`
    ///
    /// # 公式
    /// `sqrt(periods_per_year) * mean(log_return) / std(log_return)`
    pub fn sharpe_ratio(&self, periods_per_year: f64) -> f64 {
        let n = self.log_return_count.load(Ordering::Relaxed);
        if n < 2 {
            return 0.0;
        }
        if n < 30 {
            // 0.7.1 PR-D:统计意义不足时 warn,而不是静默 0
            tracing::warn!(
                n,
                "sharpe_ratio has weak statistical significance (n={} < 30 samples), result may be misleading",
                n
            );
        }
        let n_f = n as f64;
        let mean = self.log_return_sum.load(Ordering::Relaxed) as f64 / 1e9 / n_f;
        // sum_sq 单位是 1e18,所以 sum_sq / 1e18 / n = E[lr²]
        let var =
            (self.log_return_sq_sum.load(Ordering::Relaxed) as f64 / 1e18 / n_f) - mean * mean;
        if var <= 0.0 {
            return 0.0;
        }
        mean / var.sqrt() * periods_per_year.sqrt()
    }

    /// 0.7.1 新增:便捷夏普比率(传 bar 持续秒数,自动算年化因子)
    ///
    /// 比手算 `sharpe_ratio(periods_per_year)` 更安全(避免漏乘 `sqrt`,
    /// 避免 `35_040_f64.sqrt()` 这种 0.7.0 错传 bug)。
    ///
    /// # Args
    /// - `bar_duration_secs`: 单根 bar 持续秒数,常见值:
    ///   - 15-min bar → `900.0`
    ///   - 1h bar    → `3_600.0`
    ///   - 1d bar    → `86_400.0`
    ///   - 1min bar  → `60.0`
    ///
    /// # 边界
    /// - `bar_duration_secs <= 0.0` → 返回 `0.0`(避免除零 NaN)
    ///
    /// # Example
    /// ```ignore
    /// let m = TradingMetrics::new();
    /// // ... record some trades
    /// let s = m.sharpe_ratio_annualized(900.0);  // 15-min bar
    /// ```
    pub fn sharpe_ratio_annualized(&self, bar_duration_secs: f64) -> f64 {
        // 0.7.1 PR-D:防御性检查,避免 0 / 负数 / NaN 间隔除零
        // 用 `is_nan() || <= 0.0` 而非 `!(x > 0.0)`,因为 f64 部分有序,
        // 后者在 NaN 时会反转为 true(隐式行为),clippy::neg_cmp_op_on_partial_ord 告警
        if bar_duration_secs.is_nan() || bar_duration_secs <= 0.0 {
            return 0.0;
        }
        const SECS_PER_YEAR: f64 = 365.0 * 24.0 * 3600.0;
        let periods_per_year = SECS_PER_YEAR / bar_duration_secs;
        self.sharpe_ratio(periods_per_year)
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
        let sum_sq = m
            .log_return_sq_sum
            .load(std::sync::atomic::Ordering::Relaxed);
        let mean = sum_lr as f64 / 1e9 / n as f64;
        let e_lr2 = sum_sq as f64 / 1e18 / n as f64;
        let var = e_lr2 - mean * mean;
        assert!(
            s > 0.0,
            "正收益的 sharpe 应该 > 0,实际 {} (n={} sum_lr={} sum_sq={} mean={} e_lr2={} var={})",
            s,
            n,
            sum_lr,
            sum_sq,
            mean,
            e_lr2,
            var
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

    // ─── 0.7.1 PR-D:样本不足警告 + 便捷年化方法 ────────────────────

    /// n=1:样本不足 → 0.0,无 panic
    #[test]
    fn test_sharpe_single_sample_returns_zero() {
        let m = TradingMetrics::new();
        m.record_log_return(100_000_000); // 0.1
        assert_eq!(m.sharpe_ratio(252.0), 0.0);
    }

    /// 0.7.1:便捷方法 sharpe_ratio_annualized(secs) 与手算 sharpe_ratio(periods_per_year) 数值一致
    /// - 15-min bar → 900s → 35_040 bars/year
    /// - 1h bar    → 3600s → 8_760 bars/year
    /// - 1d bar    → 86_400s → 365 bars/year
    #[test]
    fn test_sharpe_annualized_matches_manual() {
        let m = TradingMetrics::new();
        // log_return: [0.1, 0.2, 0.15, 0.18, 0.12, 0.16, 0.14, 0.19, 0.13, 0.17]
        for v in [0.1, 0.2, 0.15, 0.18, 0.12, 0.16, 0.14, 0.19, 0.13, 0.17] {
            m.record_log_return((v * 1e9) as i64);
        }
        // 15-min bar: 35_040 bars/year
        let via_annualized_15m = m.sharpe_ratio_annualized(900.0);
        let via_manual_15m = m.sharpe_ratio(35_040.0);
        assert!(
            (via_annualized_15m - via_manual_15m).abs() < 1e-9,
            "sharpe_ratio_annualized(900) 应 == sharpe_ratio(35040), got {} vs {}",
            via_annualized_15m,
            via_manual_15m
        );

        // 1h bar: 8_760 bars/year
        let via_annualized_1h = m.sharpe_ratio_annualized(3_600.0);
        let via_manual_1h = m.sharpe_ratio(8_760.0);
        assert!(
            (via_annualized_1h - via_manual_1h).abs() < 1e-9,
            "sharpe_ratio_annualized(3600) 应 == sharpe_ratio(8760), got {} vs {}",
            via_annualized_1h,
            via_manual_1h
        );

        // 1d bar: 365 bars/year
        let via_annualized_1d = m.sharpe_ratio_annualized(86_400.0);
        let via_manual_1d = m.sharpe_ratio(365.0);
        assert!(
            (via_annualized_1d - via_manual_1d).abs() < 1e-9,
            "sharpe_ratio_annualized(86400) 应 == sharpe_ratio(365), got {} vs {}",
            via_annualized_1d,
            via_manual_1d
        );

        // 数量级关系:15min 的 annualized 应该 = 1h 的 * sqrt(35040/8760) = 1h 的 * 2
        let ratio = via_annualized_15m / via_annualized_1h;
        let expected = (35_040.0_f64 / 8_760.0).sqrt();
        assert!(
            (ratio - expected).abs() < 1e-9,
            "15min annualized / 1h annualized 应 = sqrt(35040/8760)={}, got {}",
            expected,
            ratio
        );
    }

    /// 0.7.1:边界 — bar_duration_secs=0.0 会除零,需安全处理
    /// (返回 0.0 而不是 NaN/Inf,让上层能判别)
    #[test]
    fn test_sharpe_annualized_zero_interval_safe() {
        let m = TradingMetrics::new();
        for v in [0.1, 0.2, 0.15] {
            m.record_log_return((v * 1e9) as i64);
        }
        let r = m.sharpe_ratio_annualized(0.0);
        assert!(!r.is_nan(), "bar_duration_secs=0 不应得 NaN, got {}", r);
        assert!(
            r.is_finite() || r == 0.0,
            "0 间隔应返回 0 或有限值, got {}",
            r
        );
    }

    /// 0.7.1:边界 — 样本数为 0 调用便捷方法
    #[test]
    fn test_sharpe_annualized_no_samples() {
        let m = TradingMetrics::new();
        // 0 样本 → 0.0
        assert_eq!(m.sharpe_ratio_annualized(900.0), 0.0);
    }
}
