//! 流式回测实时指标采集
//!
//! 0.4.0 新增:对齐 [`axon_core::metrics::TradingMetrics`] + [`crate::engine::BacktestEngine::RunResult`] 字段。
//!
//! ## 字段对照
//!
//! | `StreamingMetrics` 字段 | 对应 `BacktestEngine::RunResult` 字段 |
//! |------------------------|--------------------------------------|
//! | `equity_curve` | `equity_curve` |
//! | `nav_peak` | `nav_peak` |
//! | `total_pnl` (派生) | `total_pnl` |
//! | `total_fees` (经 `trading_metrics`) | `total_fees` |
//! | `win_rate` (经 `trading_metrics`) | `win_rate` |
//! | `sharpe_ratio` (经 `trading_metrics`,默认 252 年化) | `sharpe_ratio` |
//! | `max_drawdown` (扫描 `equity_curve`) | `max_drawdown` |
//! | `max_drawdown_pct` | `max_drawdown_pct` |
//!
//! ## 设计要点
//!
//! - **不依赖 `StreamingEngine`**:`StreamingMetrics` 是纯数据收集器,由 `StreamingEngine`
//!   在 fill 路径调用 `record_fill` 推进;避免 metrics 模块和 engine 模块循环依赖
//! - **复用 `TradingMetrics`**:胜率/夏普/累计 pnl/fees 直接走 `axon_core::metrics::TradingMetrics`,
//!   线程安全(AtomicI64)无锁
//! - **NAV 峰值实时维护**:`record_fill` 内 O(1) 更新,`max_drawdown` 派生时 O(n) 扫描

use axon_core::metrics::TradingMetrics;
use axon_core::time::Timestamp;

/// 权益曲线采样点
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EquityPoint {
    /// 采样时间戳
    pub timestamp: Timestamp,
    /// 当时 NAV
    pub nav: f64,
}

/// 流式回测实时指标收集器
///
/// 每笔 fill 后由 [`crate::streaming::StreamingEngine`] 调用 [`Self::record_fill`] 推进。
/// 终态通过 [`Self::snapshot`] 导出 [`StreamingMetricsSnapshot`],由
/// `StreamingEngine::metrics_snapshot` 拼装到 [`crate::streaming::StreamingSnapshot`]。
#[derive(Debug)]
pub struct StreamingMetrics {
    /// 交易指标(胜率/夏普/累计 pnl/fees)— 线程安全,无锁
    trading_metrics: TradingMetrics,
    /// 权益曲线采样(每笔 fill 后追加一点)
    equity_curve: Vec<EquityPoint>,
    /// NAV 历史峰值
    nav_peak: f64,
    /// 初始资金(用于 `total_pnl = current_nav - initial_cash` 派生)
    initial_cash: f64,
    /// 上次 NAV(用于 log return 计算)
    prev_nav: Option<f64>,
}

impl Default for StreamingMetrics {
    fn default() -> Self {
        Self {
            trading_metrics: TradingMetrics::new(),
            equity_curve: Vec::new(),
            nav_peak: 0.0,
            initial_cash: 0.0,
            prev_nav: None,
        }
    }
}

impl StreamingMetrics {
    /// 创建新收集器(初始资金 = 0)
    pub fn new() -> Self {
        Self::default()
    }

    /// 创建带初始资金的收集器
    ///
    /// `initial_cash` 决定 `total_pnl` 派生基准;通常在 `deposit` 后调用
    pub fn with_initial_cash(initial_cash: f64) -> Self {
        Self {
            initial_cash,
            ..Self::default()
        }
    }

    /// 设置初始资金(后续覆盖)
    #[allow(dead_code)] // 0.4.0 S4 集成到 StreamingEngine 后从外部调,先静默
    pub fn set_initial_cash(&mut self, initial_cash: f64) {
        self.initial_cash = initial_cash;
    }

    /// 记录单笔 fill:更新 NAV / equity_curve / trading_metrics
    ///
    /// # 参数
    ///
    /// - `pnl`:本笔 fill 的 PnL(× 1e6 定点)
    /// - `fee`:本笔 fill 的手续费(× 1e6 定点)
    /// - `nav`:fill 后投资组合 NAV(f64)
    /// - `timestamp`:fill 时间戳
    pub fn record_fill(&mut self, pnl: i64, fee: i64, nav: f64, timestamp: Timestamp) {
        // 1. 累计 trade metrics(wins/losses/fees/pnl)
        self.trading_metrics.record_trade(pnl, fee);

        // 2. log return(供 Sharpe 计算)— 跳过 prev <= 0 防御
        if let Some(prev) = self.prev_nav
            && prev > 0.0
            && nav > 0.0
        {
            let lr = (nav / prev).ln();
            // lr 通常在 [-1, 1],× 1e9 仍在 i64 安全范围
            self.trading_metrics.record_log_return((lr * 1e9) as i64);
        }

        // 3. NAV 峰值实时维护
        if nav > self.nav_peak {
            self.nav_peak = nav;
        }

        // 3.5  0.8.0 B5:NAV 累加器(供 calmar_ratio 用)
        self.trading_metrics.record_nav(nav);

        // 4. equity_curve 采样
        self.equity_curve.push(EquityPoint { timestamp, nav });

        // 5. 更新 prev_nav
        self.prev_nav = Some(nav);
    }

    /// 权益曲线副本
    pub fn equity_curve(&self) -> &[EquityPoint] {
        &self.equity_curve
    }

    /// 底层 TradingMetrics 引用
    #[allow(dead_code)] // 0.4.0 S4 集成后通过 streaming::StreamingEngine 暴露给上层
    pub fn trading_metrics(&self) -> &TradingMetrics {
        &self.trading_metrics
    }

    /// NAV 历史峰值
    pub fn nav_peak(&self) -> f64 {
        self.nav_peak
    }

    /// 初始资金
    pub fn initial_cash(&self) -> f64 {
        self.initial_cash
    }

    /// 总 PnL(`current_nav - initial_cash`)
    pub fn total_pnl(&self, current_nav: f64) -> f64 {
        current_nav - self.initial_cash
    }

    /// 最大回撤(USD,绝对值)— 沿 equity_curve 单次扫描
    pub fn max_drawdown(&self) -> f64 {
        let mut peak = self.equity_curve.first().map(|p| p.nav).unwrap_or(0.0);
        let mut max_dd = 0.0;
        for p in &self.equity_curve {
            if p.nav > peak {
                peak = p.nav;
            }
            let dd = peak - p.nav;
            if dd > max_dd {
                max_dd = dd;
            }
        }
        max_dd
    }

    /// 最大回撤百分比(`max_drawdown / nav_peak`,0~1)
    pub fn max_drawdown_pct(&self) -> f64 {
        if self.nav_peak <= 0.0 {
            0.0
        } else {
            self.max_drawdown() / self.nav_peak
        }
    }

    /// 导出纯 metrics 快照(不含 portfolio 状态/mode)
    ///
    /// `current_nav` 由调用方传入(通常为 `portfolio.nav()`);
    /// `periods_per_year` 默认 252(bar 频率,本工程主用日级)
    pub fn snapshot(&self, current_nav: f64, periods_per_year: f64) -> StreamingMetricsSnapshot {
        StreamingMetricsSnapshot {
            total_pnl: self.total_pnl(current_nav),
            total_fees: self.trading_metrics.total_fees_f64(),
            win_rate: self.trading_metrics.win_rate(),
            sharpe_ratio: self.trading_metrics.sharpe_ratio(periods_per_year),
            max_drawdown: self.max_drawdown(),
            max_drawdown_pct: self.max_drawdown_pct(),
            nav_peak: self.nav_peak,
            final_nav: current_nav,
            equity_curve_len: self.equity_curve.len(),
        }
    }
}

/// `StreamingMetrics::snapshot` 的产物 — 纯 metrics 视图
///
/// 与 `StreamingSnapshot`(在 `engine.rs`)的区别:不包含 portfolio nav / mode / active_orders 等运行时字段
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StreamingMetricsSnapshot {
    /// 总 PnL(`final_nav - initial_cash`)
    pub total_pnl: f64,
    /// 总手续费
    pub total_fees: f64,
    /// 胜率(来自 TradingMetrics)
    pub win_rate: f64,
    /// 夏普比率(默认 252 年化)
    pub sharpe_ratio: f64,
    /// 最大回撤(USD 绝对值)
    pub max_drawdown: f64,
    /// 最大回撤百分比(0~1)
    pub max_drawdown_pct: f64,
    /// NAV 历史峰值
    pub nav_peak: f64,
    /// 终态 NAV
    pub final_nav: f64,
    /// equity_curve 长度(避免复制大数组)
    pub equity_curve_len: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::time::Timestamp;

    fn ts(nanos: i64) -> Timestamp {
        Timestamp::from_nanos(nanos)
    }

    #[test]
    fn empty_metrics_zero_drawdown() {
        let m = StreamingMetrics::new();
        assert_eq!(m.max_drawdown(), 0.0);
        assert_eq!(m.max_drawdown_pct(), 0.0);
        assert_eq!(m.equity_curve().len(), 0);
        assert_eq!(m.nav_peak(), 0.0);
        assert_eq!(m.initial_cash(), 0.0);
    }

    #[test]
    fn record_fill_appends_equity_curve_point() {
        let mut m = StreamingMetrics::new();
        m.record_fill(100_000, 10_000, 100_000.0, ts(1_000));
        m.record_fill(50_000, 10_000, 100_050.0, ts(2_000));
        assert_eq!(m.equity_curve().len(), 2);
        assert_eq!(m.equity_curve()[0].nav, 100_000.0);
        assert_eq!(m.equity_curve()[1].nav, 100_050.0);
    }

    #[test]
    fn nav_peak_tracks_high_water_mark() {
        let mut m = StreamingMetrics::new();
        m.record_fill(0, 0, 100.0, ts(1));
        m.record_fill(0, 0, 200.0, ts(2));
        m.record_fill(0, 0, 150.0, ts(3));
        assert_eq!(m.nav_peak(), 200.0);
    }

    #[test]
    fn max_drawdown_finds_largest_peak_to_trough() {
        let mut m = StreamingMetrics::new();
        // NAV 序列:100 → 200 → 150 → 100 → 250 → 200
        // peak 序列:100 → 200 → 200 → 200 → 250 → 250
        // drawdown:0 → 0 → 50 → 100 → 0 → 50
        // max_dd = 100
        for (i, nav) in [100.0, 200.0, 150.0, 100.0, 250.0, 200.0]
            .iter()
            .enumerate()
        {
            m.record_fill(0, 0, *nav, ts(i as i64));
        }
        assert_eq!(m.nav_peak(), 250.0);
        assert!((m.max_drawdown() - 100.0).abs() < 1e-9);
        assert!((m.max_drawdown_pct() - 100.0 / 250.0).abs() < 1e-9);
    }

    #[test]
    fn total_pnl_uses_initial_cash() {
        let mut m = StreamingMetrics::with_initial_cash(100_000.0);
        m.record_fill(0, 0, 100_000.0, ts(1));
        m.record_fill(500_000, 0, 100_500.0, ts(2));
        assert!((m.total_pnl(100_500.0) - 500.0).abs() < 1e-9);
    }

    #[test]
    fn win_rate_and_total_fees_propagate_to_trading_metrics() {
        let mut m = StreamingMetrics::new();
        // 1 赢 + 1 输,win_rate = 0.5
        m.record_fill(100_000, 50_000, 100.0, ts(1));
        m.record_fill(-50_000, 50_000, 99.5, ts(2));
        let snap = m.snapshot(99.5, 252.0);
        assert!((snap.win_rate - 0.5).abs() < 1e-9);
        // fee: 50_000 (× 1e6) + 50_000 = 0.1 f64
        assert!((snap.total_fees - 0.1).abs() < 1e-6);
        assert_eq!(snap.equity_curve_len, 2);
    }
}
