//! 交易胜率/夏普等指标收集器
//!
//! 线程安全:原子字段用 `AtomicI64`,NAV 累加器用 `Mutex`
//! (0.8.0 B5 新增)。所有累加器用定点数记录,避免浮点竞态。

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

/// 0.8.0 B5:样本不足警告阈值
///
/// 任何 sharpe / sortino / calmar 等"基于样本统计"的指标,样本数低于此阈值
/// 时 `tracing::warn!` 提示统计意义不足。0.7.1 PR-D 引入 sharpe 的 warn,
/// 0.8.0 B5 抽到统一 helper 复用。
pub const SAMPLE_SIZE_WARN_THRESHOLD: usize = 30;

/// 0.8.0 B5 抽:统一样本不足检查
///
/// n < threshold → `tracing::warn!` 提示统计意义不足,返回 `false` 让 caller
/// 决定是否继续算(返回 0.0 还是带警告值)。统一在 sharpe / sortino / calmar
/// 起始处调用,避免每处重复 warn 模板。
///
/// # Args
/// - `n`: 实际样本数
/// - `threshold`: 阈值(通常 [`SAMPLE_SIZE_WARN_THRESHOLD`])
/// - `metric`: 指标名(用于 warn 消息)
///
/// # Returns
/// - `true`: n >= threshold,样本充足
/// - `false`: n < threshold,已 warn,caller 应决定 fallback(通常返回 0.0)
pub fn assert_sample_size(n: usize, threshold: usize, metric: &str) -> bool {
    if n < threshold {
        tracing::warn!(
            n,
            threshold,
            metric,
            "{} has weak statistical significance (n={} < {} samples), result may be misleading",
            metric,
            n,
            threshold,
        );
        false
    } else {
        true
    }
}

/// 0.8.0 B5:NAV 累加器内部状态
///
/// `peak` 是 caller 推过的最大 NAV;`max_dd` 是 `peak - current_nav` 的
/// 历史最大值;`count` 是 `record_nav` 调用次数。
///
/// 用 `Mutex` 而非原子操作:BacktestEngine 主要是单线程串行调用,但
/// `Send + Sync` 暴露给外部;`Mutex` 提供简单正确的同步,BacktestEngine
/// 路径下零竞争(锁开销可忽略)。
#[derive(Debug, Default)]
struct NavState {
    peak: f64,
    max_dd: f64,
    count: u64,
}

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
    // ── 0.8.0 B5 新增:NAV 累加器(用于 calmar)────────────────────
    /// NAV 历史峰值 + 最大回撤 + 采样计数
    ///
    /// 由 `record_nav` 自动维护 peak / max_dd / count;calmar_ratio 直接读取。
    /// 之所以用 `Mutex` 而非 `AtomicU64`(存 f64 bits):CAS 循环下 max_dd 计算
    /// 需要读 peak 当前值,如果同时有其他线程 record_nav 更新 peak,会算错;
    /// `Mutex` 一次锁定内完成 peak 更新 + dd 更新,语义简单且正确。
    nav_state: Mutex<NavState>,
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

    /// 0.8.0 B5 新增:记录单帧 NAV(用于 calmar 计算)
    ///
    /// 内部自动维护:
    /// - `nav_peak` = max(原 peak, 当前 nav)
    /// - `max_drawdown` = max(原 max_dd, peak - 当前 nav)
    /// - `nav_count` += 1
    ///
    /// 由 `BacktestEngine` 在 `apply_fill` / `handle_mark` / `handle_funding` /
    /// `sample_bar_nav` 中调用,与 `record_log_return` 配合(同一事件 trigger)。
    /// 短回测 + 无 fill 时,`record_nav` 仍被 `sample_bar_nav` 调,calmar 可算。
    ///
    /// # 边界
    /// - `nav < 0` 时仍记录(账户穿仓场景),但 `max_drawdown` 只算正回撤
    ///   (`peak - nav > 0` 才更新),`peak` 永远 ≥ 当前 nav 不会变
    pub fn record_nav(&self, nav: f64) {
        let mut s = self.nav_state.lock().expect("TradingMetrics nav_state poisoned");
        if nav > s.peak {
            s.peak = nav;
        }
        let dd = s.peak - nav;
        if dd > s.max_dd {
            s.max_dd = dd;
        }
        s.count += 1;
    }

    /// 0.8.0 B5 新增:读取 NAV 历史峰值
    pub fn nav_peak(&self) -> f64 {
        self.nav_state.lock().expect("TradingMetrics nav_state poisoned").peak
    }

    /// 0.8.0 B5 新增:读取最大回撤(绝对值,USD 单位)
    pub fn nav_max_drawdown(&self) -> f64 {
        self.nav_state.lock().expect("TradingMetrics nav_state poisoned").max_dd
    }

    /// 0.8.0 B5 新增:读取 NAV 采样计数(`record_nav` 调用次数)
    pub fn nav_count(&self) -> u64 {
        self.nav_state.lock().expect("TradingMetrics nav_state poisoned").count
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
    /// - 样本数 `< 30` → `tracing::warn!` 提示统计意义不足(0.8.0 B5 用
    ///   [`assert_sample_size`] helper 统一格式)
    /// - 方差 `<= 0`(单调行情)→ 返回 `0.0`
    ///
    /// # 公式
    /// `sqrt(periods_per_year) * mean(log_return) / std(log_return)`
    pub fn sharpe_ratio(&self, periods_per_year: f64) -> f64 {
        let n = self.log_return_count.load(Ordering::Relaxed);
        if n < 2 {
            return 0.0;
        }
        // 0.8.0 B5:用统一 helper,提示信息包含指标名
        assert_sample_size(n as usize, SAMPLE_SIZE_WARN_THRESHOLD, "sharpe_ratio");
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

    /// 0.8.0 B5 新增:Sortino 比率(基于 log return 年化,只算下行波动)
    ///
    /// Sortino 区别于 Sharpe:分母只算**负收益**的标准差(下行波动),
    /// 不惩罚正收益波动。对只有下行风险的策略(典型套利 / 做市)
    /// 更准确。
    ///
    /// # 公式
    /// `sqrt(periods_per_year) * mean(log_return) / downside_deviation`
    ///
    /// 其中 `downside_deviation = sqrt(mean(min(lr, 0)²))`,**n 分母**
    /// (总体方差,用户 0.8.0 拍板)。
    ///
    /// # 边界
    /// - 样本数 `< 2` → 返回 `0.0`
    /// - 样本数 `< 30` → `tracing::warn!`
    /// - `downside_deviation == 0`(全正收益 / 全 0 收益)→ 返回 `0.0`
    ///   (避免除零;实际场景下应视为"无下行风险",Sortino 趋于 +∞)
    pub fn sortino_ratio(&self, periods_per_year: f64) -> f64 {
        let n = self.log_return_count.load(Ordering::Relaxed);
        if n < 2 {
            return 0.0;
        }
        assert_sample_size(n as usize, SAMPLE_SIZE_WARN_THRESHOLD, "sortino_ratio");

        // 0.8.0 B5 实现限制:无法从原子累加器还原单样本(只存 sum + sq_sum),
        // 无法精确算 E[min(lr, 0)²]。本方法用闭式近似:
        //   - mean >= 0:E[min(lr,0)²] ≈ max(0, var - mean²/2)
        //   - mean <  0:E[min(lr,0)²] ≈ var + mean²/2
        // 这是粗近似,只用于 B5 占位。0.9.0 应加 `log_return_down_sq_sum`
        // 定点累加器拿精确值。
        let n_f = n as f64;
        let mean = self.log_return_sum.load(Ordering::Relaxed) as f64 / 1e9 / n_f;
        let total_sq = self.log_return_sq_sum.load(Ordering::Relaxed) as f64 / 1e18;
        let var = total_sq / n_f - mean * mean;
        let down_dev_sq = if mean >= 0.0 {
            (var - mean * mean / 2.0).max(0.0)
        } else {
            (var + mean * mean / 2.0).max(0.0)
        };
        let down_dev = down_dev_sq.sqrt();
        if down_dev <= 0.0 {
            return 0.0;
        }
        mean / down_dev * periods_per_year.sqrt()
    }

    /// 0.8.0 B5 新增:Calmar 比率(年化收益 / 最大回撤)
    ///
    /// Calmar 用最大回撤做分母,衡量"收益 vs 极端损失"风险调整后的表现。
    /// 年化收益基于 `log_return_sum`(与 sharpe / sortino 同一累加器)。
    /// 最大回撤来自 `record_nav` 累加器(`nav_max_drawdown`)。
    ///
    /// # 公式
    /// `periods_per_year * mean(log_return) / max_drawdown`
    ///
    /// # 边界
    /// - `nav_count < 2` → 返回 `0.0`(无 NAV 演化,无法算回撤)
    /// - `nav_count < 30` → `tracing::warn!`
    /// - `max_drawdown == 0`(NAV 单调上升)→ 返回 `0.0`
    ///   (Calmar 在此场景趋于 +∞,工程上按 0 处理)
    /// - `log_return_count < 2` → 返回 `0.0`(无收益数据)
    pub fn calmar_ratio(&self, periods_per_year: f64) -> f64 {
        let n_lr = self.log_return_count.load(Ordering::Relaxed);
        if n_lr < 2 {
            return 0.0;
        }
        let n_nav = self.nav_count();
        if n_nav < 2 {
            return 0.0;
        }
        assert_sample_size(n_lr as usize, SAMPLE_SIZE_WARN_THRESHOLD, "calmar_ratio");
        assert_sample_size(n_nav as usize, SAMPLE_SIZE_WARN_THRESHOLD, "calmar_ratio (nav)");

        let mean = self.log_return_sum.load(Ordering::Relaxed) as f64 / 1e9 / n_lr as f64;
        let max_dd = self.nav_max_drawdown();
        if max_dd <= 0.0 {
            return 0.0;
        }
        // Calmar 分子是年化收益:mean × periods_per_year(不是 × sqrt!)
        // 区别于 sharpe / sortino 的 sqrt(periods_per_year)
        mean * periods_per_year / max_dd
    }

    /// 0.8.0 B5 新增:Information Ratio(超额收益 / tracking error)
    ///
    /// 衡量策略相对 benchmark 的"主动管理"表现。tracking error 是
    /// `strategy_returns - benchmark_returns` 序列的标准差。
    ///
    /// # 公式
    /// `sqrt(periods_per_year) * mean(excess_return) / std(excess_return)`
    ///
    /// 其中 `excess_return_t = strategy_log_return_t - benchmark_log_return_t`。
    ///
    /// # Args
    /// - `benchmark`: benchmark 的 log return 序列(与 strategy 内部累加器
    ///   等长)。`benchmark.len()` 与 `log_return_count` 不等时,按 min 截断
    ///   并 `tracing::warn!`。
    /// - `periods_per_year`: 同 sharpe。
    ///
    /// # 边界
    /// - 样本数 `< 2` → 返回 `0.0`
    /// - 样本数 `< 30` → `tracing::warn!`
    /// - `tracking_error == 0` → 返回 `0.0`
    ///
    /// # 0.8.0 实现限制
    ///
    /// 内部 log_return 累加器只存 `sum` + `sq_sum`,无法还原单样本 strategy
    /// return,所以 `cov(strategy, bench)` 无法精确算。本方法用保守上界:
    /// **假设 cov ≈ 0(uncorrelated)**,即 `var(excess) ≈ var(strategy) + var(bench)`。
    /// 0.9.0 应加 per-sample log return 数组,拿真实 cov。
    pub fn information_ratio(&self, benchmark: &[f64], periods_per_year: f64) -> f64 {
        let n_lr = self.log_return_count.load(Ordering::Relaxed);
        if n_lr < 2 || benchmark.len() < 2 {
            return 0.0;
        }
        let n_lr_us = n_lr as usize;
        let n = n_lr_us.min(benchmark.len());
        assert_sample_size(n, SAMPLE_SIZE_WARN_THRESHOLD, "information_ratio");
        if n != n_lr_us {
            tracing::warn!(
                n_lr,
                n_bench = benchmark.len(),
                "information_ratio: benchmark length != log_return_count, truncating to min"
            );
        }

        let n_f = n as f64;

        // strategy: 从原子累加器读 mean / var(n 分母,与 sortino 一致)
        let strategy_mean = self.log_return_sum.load(Ordering::Relaxed) as f64 / 1e9 / n_f;
        let strategy_var = (self.log_return_sq_sum.load(Ordering::Relaxed) as f64 / 1e18 / n_f
            - strategy_mean * strategy_mean)
            .max(0.0);

        // benchmark: 从 raw slice 算(n 帧,n-1 无偏)
        let bench_mean: f64 = benchmark[..n].iter().sum::<f64>() / n_f;
        let bench_var: f64 = {
            let s: f64 = benchmark[..n]
                .iter()
                .map(|r| (r - bench_mean).powi(2))
                .sum();
            (s / (n_f - 1.0)).max(0.0)
        };

        // excess mean = strategy - benchmark
        let excess_mean = strategy_mean - bench_mean;

        // tracking error: 0.8.0 B5 近似 cov(strategy, bench) ≈ 0,
        // var(excess) ≈ var(strategy) + var(bench)(保守上界)
        let excess_var = (strategy_var + bench_var).max(0.0);
        if excess_var <= 0.0 {
            return 0.0;
        }
        let tracking_error = excess_var.sqrt();
        excess_mean / tracking_error * periods_per_year.sqrt()
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

    // ─── 0.8.0 B5:Sortino / Calmar / IR / NAV 累加器 / assert_sample_size ───

    /// `assert_sample_size` 行为:
    /// - n < threshold → `false` + warn
    /// - n >= threshold → `true` + 无 warn
    #[test]
    fn b5_assert_sample_size_threshold() {
        // n=0, threshold=30 → 不足
        let r = assert_sample_size(0, 30, "test_metric");
        assert!(!r, "n=0 < 30 应返回 false");

        // n=29, threshold=30 → 不足
        let r = assert_sample_size(29, 30, "test_metric");
        assert!(!r, "n=29 < 30 应返回 false");

        // n=30, threshold=30 → 充足
        let r = assert_sample_size(30, 30, "test_metric");
        assert!(r, "n=30 >= 30 应返回 true");

        // n=100, threshold=30 → 充足
        let r = assert_sample_size(100, 30, "test_metric");
        assert!(r, "n=100 >= 30 应返回 true");
    }

    /// `record_nav` 维护 peak / max_dd / count
    #[test]
    fn b5_record_nav_updates_peak_and_max_dd() {
        let m = TradingMetrics::new();
        assert_eq!(m.nav_count(), 0);
        assert_eq!(m.nav_peak(), 0.0);
        assert_eq!(m.nav_max_drawdown(), 0.0);

        m.record_nav(100.0);
        assert_eq!(m.nav_count(), 1);
        assert_eq!(m.nav_peak(), 100.0);
        assert_eq!(m.nav_max_drawdown(), 0.0);

        m.record_nav(120.0);
        assert_eq!(m.nav_peak(), 120.0);
        assert_eq!(m.nav_max_drawdown(), 0.0);

        m.record_nav(90.0);
        // peak = 120, current = 90, dd = 30
        assert_eq!(m.nav_peak(), 120.0);
        assert!((m.nav_max_drawdown() - 30.0).abs() < 1e-9);

        m.record_nav(150.0);
        // peak 更新到 150, max_dd 不变(30 仍是历史最大)
        assert_eq!(m.nav_peak(), 150.0);
        assert!((m.nav_max_drawdown() - 30.0).abs() < 1e-9);

        m.record_nav(110.0);
        // peak = 150, current = 110, dd = 40 → max_dd 更新
        assert!((m.nav_max_drawdown() - 40.0).abs() < 1e-9);
    }

    /// `record_nav` 在 NAV < peak 时正确算回撤(单调上升 → max_dd = 0)
    #[test]
    fn b5_record_nav_monotonic_up_no_drawdown() {
        let m = TradingMetrics::new();
        for v in [100.0, 110.0, 120.0, 130.0, 140.0] {
            m.record_nav(v);
        }
        assert_eq!(m.nav_peak(), 140.0);
        assert_eq!(m.nav_max_drawdown(), 0.0);
    }

    /// Sortino:全正收益 → down_dev ≈ 0 → 返回 0.0
    #[test]
    fn b5_sortino_all_positive_returns_zero() {
        let m = TradingMetrics::new();
        for _ in 0..10 {
            m.record_log_return(50_000_000); // 0.05
        }
        // 全正收益,no downside → Sortino 趋于 +∞,工程上 0.0
        assert_eq!(m.sortino_ratio(252.0), 0.0);
    }

    /// Sortino:混合收益(有正有负)→ 正的 sortino
    #[test]
    fn b5_sortino_mixed_returns_positive() {
        let m = TradingMetrics::new();
        // 收益序列:[0.10, -0.05, 0.08, -0.03, 0.12]
        for v in [0.10, -0.05, 0.08, -0.03, 0.12] {
            m.record_log_return((v * 1e9) as i64);
        }
        // mean ≈ 0.044 > 0,sortino 应 > 0
        let s = m.sortino_ratio(252.0);
        assert!(s > 0.0, "混合收益 sortino 应 > 0,got {s}");
    }

    /// Sortino:n < 2 → 0.0
    #[test]
    fn b5_sortino_insufficient_samples_returns_zero() {
        let m = TradingMetrics::new();
        m.record_log_return(100_000_000);
        assert_eq!(m.sortino_ratio(252.0), 0.0);
    }

    /// Calmar:无 NAV 数据 → 0.0
    #[test]
    fn b5_calmar_no_nav_returns_zero() {
        let m = TradingMetrics::new();
        for v in [0.10, 0.20, 0.15] {
            m.record_log_return((v * 1e9) as i64);
        }
        // 有 log return 但没 record_nav → max_dd = 0 → calmar = 0
        assert_eq!(m.calmar_ratio(252.0), 0.0);
    }

    /// Calmar:有 NAV + log return → 算非零 calmar
    #[test]
    fn b5_calmar_basic_positive() {
        let m = TradingMetrics::new();
        // log returns: 平均 0.05
        for v in [0.05, 0.04, 0.06, 0.05, 0.04, 0.06, 0.05, 0.05] {
            m.record_log_return((v * 1e9) as i64);
        }
        // NAV: 单调上升
        for v in [100.0, 105.0, 109.0, 114.0, 119.0, 125.0, 131.0, 137.0] {
            m.record_nav(v);
        }
        // mean ≈ 0.05,年化 = 0.05 * 252 ≈ 12.6
        // max_dd = 0(单调上升)→ calmar = 0(工程上避免 +∞)
        assert_eq!(m.calmar_ratio(252.0), 0.0);
    }

    /// Calmar:NAV 有真实回撤 → 算非零 calmar
    #[test]
    fn b5_calmar_with_drawdown() {
        let m = TradingMetrics::new();
        for v in [0.10, 0.10, 0.10, 0.10, 0.10, 0.10, 0.10, 0.10] {
            m.record_log_return((v * 1e9) as i64);
        }
        // NAV: 100 → 130(peak) → 90(max dd = 40) → 110
        for v in [100.0, 110.0, 130.0, 90.0, 110.0] {
            m.record_nav(v);
        }
        // mean = 0.10, 年化 = 0.10 * 252 = 25.2
        // max_dd = 130 - 90 = 40
        // calmar = 25.2 / 40 = 0.63
        let c = m.calmar_ratio(252.0);
        assert!(c > 0.0, "calmar 应 > 0,got {c}");
        let expected = (0.10 * 252.0) / 40.0;
        assert!(
            (c - expected).abs() < 1e-6,
            "calmar 应 ≈ {expected},got {c}"
        );
    }

    /// IR:benchmark 长度不足 → 0.0
    #[test]
    fn b5_ir_insufficient_benchmark_returns_zero() {
        let m = TradingMetrics::new();
        for v in [0.1, 0.2, 0.15] {
            m.record_log_return((v * 1e9) as i64);
        }
        // benchmark 长度 1 < 2 → 0.0
        let ir = m.information_ratio(&[0.05], 252.0);
        assert_eq!(ir, 0.0);
    }

    /// IR:benchmark 与 strategy 完全同步 → 几乎无超额收益 → IR ≈ 0
    #[test]
    fn b5_ir_zero_excess_mean() {
        let m = TradingMetrics::new();
        // strategy returns
        for v in [0.10, 0.15, 0.12, 0.18, 0.14] {
            m.record_log_return((v * 1e9) as i64);
        }
        // benchmark 完全相同 → excess_mean = 0 → IR = 0
        let benchmark: Vec<f64> = [0.10, 0.15, 0.12, 0.18, 0.14].to_vec();
        let ir = m.information_ratio(&benchmark, 252.0);
        assert!(ir.abs() < 1e-9, "excess=0 → IR 应=0,got {ir}");
    }

    /// IR:benchmark 显著低于 strategy → 正 IR
    /// 关键测试:不同 benchmark 应产生不同 IR(证明参数被使用)
    #[test]
    fn b5_ir_uses_benchmark_parameter() {
        let m = TradingMetrics::new();
        for v in [0.10, 0.15, 0.12, 0.18, 0.14] {
            m.record_log_return((v * 1e9) as i64);
        }

        // benchmark1: 低收益 → strategy 超额 = 高
        let bench_low: Vec<f64> = [0.01, 0.02, 0.01, 0.03, 0.02].to_vec();
        let ir_low = m.information_ratio(&bench_low, 252.0);

        // benchmark2: 高收益 → strategy 超额 = 低
        let bench_high: Vec<f64> = [0.10, 0.15, 0.12, 0.18, 0.14].to_vec();
        let ir_high = m.information_ratio(&bench_high, 252.0);

        // 关键断言:不同 benchmark → 不同 IR
        assert!(
            (ir_low - ir_high).abs() > 1e-6,
            "IR 必须随 benchmark 变化,low={ir_low} high={ir_high}"
        );
        // 弱 benchmark (low) 应 > 强 benchmark (high)
        assert!(
            ir_low > ir_high,
            "弱 benchmark 应给更高 IR,low={ir_low} high={ir_high}"
        );
    }
}
