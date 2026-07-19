//! 组合级风险敞口计算(0.7.0 Phase 4 新增)
//!
//! # 范围
//!
//! 提供 `PortfolioRiskEngine::delta_exposure` / `gamma_exposure` / `vega` 三
//! 类基础风险敞口,数据源是 `axon_core::portfolio::Portfolio.positions`。
//!
//! ## 定义(0.7.0 范围,刻意保持简单)
//!
//! - **per-leg delta** = `position.quantity`(spot / swap 线性合约,unit_delta = 1.0)
//! - **per-leg gamma** = `0.0`(无 mark 历史,无 IV,无法计算)
//! - **portfolio delta** = `Σ per_leg_delta`
//! - **vega** = `0.0`(无 IV 源)
//!
//! ## 不在本 plan 范围
//!
//! - 跨 instrument 协方差(需要 mark 历史窗口,留 0.8.0)
//! - vol-based vega(需要 IV surface,留 0.8.0)
//! - contract_size 修正(perp 合约面值,留 0.8.0)
//!
//! # 用法
//!
//! ```rust,no_run
//! use axon_risk::portfolio::PortfolioRiskEngine;
//! use axon_core::portfolio::Portfolio;
//!
//! let engine = PortfolioRiskEngine::new();
//! let portfolio = Portfolio::new(Default::default(), 0.0);
//! let delta = engine.delta_exposure(&portfolio);
//! let total = engine.portfolio_delta(&portfolio);
//! ```

use std::collections::HashMap;

use axon_core::portfolio::Portfolio;
use axon_core::types::Instrument;

use serde::{Deserialize, Serialize};

/// 0.7.0 Phase 4 新增:风险敞口报告
///
/// 由 [`BacktestEngine::run`](axon_backtest::engine::BacktestEngine::run) 填充
/// 到 [`RunResult.risk_metrics`](axon_backtest::engine::RunResult::risk_metrics),
/// 同时通过 PyO3 binding 暴露到 `run_result["risk_metrics"]` dict。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RiskMetricsReport {
    /// 每个 instrument 的 delta 暴露
    /// (`instrument -> delta`)
    pub per_leg_delta: HashMap<Instrument, f64>,
    /// 组合总 delta(Σ per-leg delta)
    pub portfolio_delta: f64,
    /// 每个 instrument 的 gamma 暴露
    /// (`instrument -> gamma`)
    pub per_leg_gamma: HashMap<Instrument, f64>,
    /// 组合总 gamma
    pub total_gamma: f64,
    /// vega(暂时 0.0,无 IV 源)
    pub vega: f64,
    /// 多 leg Sharpe(沿用 [`RunResult.sharpe_ratio`](axon_backtest::engine::RunResult::sharpe_ratio))
    pub sharpe_with_legs: f64,
}

impl RiskMetricsReport {
    /// 创建空报告
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从 per-leg map + Sharpe 计算 portfolio-level 字段
    ///
    /// 在 [`PortfolioRiskEngine::compute_report`] 中调用,本结构本身不实现计算逻辑
    /// —— 保持只读视图语义。
    pub fn aggregate(per_leg_delta: HashMap<Instrument, f64>, sharpe: f64) -> Self {
        let portfolio_delta: f64 = per_leg_delta.values().sum();
        // gamma 暂时全 0,留 0.8.0
        let per_leg_gamma: HashMap<Instrument, f64> =
            per_leg_delta.keys().map(|k| (k.clone(), 0.0)).collect();
        Self {
            per_leg_delta,
            portfolio_delta,
            per_leg_gamma,
            total_gamma: 0.0,
            vega: 0.0,
            sharpe_with_legs: sharpe,
        }
    }
}

/// 组合级风险敞口计算引擎
///
/// 0.7.0 Phase 4 新增,包装 `Portfolio.positions`,对外暴露 delta / gamma / vega
/// 三类敞口。
#[derive(Debug, Clone, Default)]
pub struct PortfolioRiskEngine {
    /// 预留配置(0.7.0 暂未使用,0.8.0 接入 contract_size / IV 源时启用)
    _private: (),
}

impl PortfolioRiskEngine {
    /// 创建新引擎
    pub fn new() -> Self {
        Self::default()
    }

    /// 计算 per-leg delta
    ///
    /// 定义:`delta[instrument] = position.quantity.as_f64()`(线性合约,unit_delta = 1.0)
    ///
    /// 返回:`HashMap<Instrument, f64>`,只包含非零持仓
    pub fn delta_exposure(&self, portfolio: &Portfolio) -> HashMap<Instrument, f64> {
        portfolio
            .positions()
            .iter()
            .filter_map(|(inst, pos)| {
                let qty = pos.quantity.as_f64();
                if qty.abs() > 1e-9 {
                    Some((inst.clone(), qty))
                } else {
                    None
                }
            })
            .collect()
    }

    /// 计算 per-leg gamma
    ///
    /// 0.7.0 范围:全部返回 `0.0`(无 mark 历史,无 IV 源)
    ///
    /// 返回:`HashMap<Instrument, f64>`,只包含非零持仓
    pub fn gamma_exposure(&self, portfolio: &Portfolio) -> HashMap<Instrument, f64> {
        portfolio
            .positions()
            .iter()
            .filter_map(|(inst, pos)| {
                if pos.quantity.as_f64().abs() > 1e-9 {
                    Some((inst.clone(), 0.0))
                } else {
                    None
                }
            })
            .collect()
    }

    /// 组合总 delta(Σ per-leg delta)
    pub fn portfolio_delta(&self, portfolio: &Portfolio) -> f64 {
        self.delta_exposure(portfolio).values().sum()
    }

    /// 组合总 gamma(0.7.0 范围:全部 0.0)
    pub fn total_gamma(&self, _portfolio: &Portfolio) -> f64 {
        0.0
    }

    /// vega(0.7.0 范围:0.0,无 IV 源)
    pub fn vega(&self, _portfolio: &Portfolio) -> f64 {
        0.0
    }

    /// 计算完整 `RiskMetricsReport`
    ///
    /// 由 [`BacktestEngine::run`](axon_backtest::engine::BacktestEngine::run)
    /// 在产出 `RunResult` 时调用。
    pub fn compute_report(&self, portfolio: &Portfolio, sharpe_ratio: f64) -> RiskMetricsReport {
        let per_leg_delta = self.delta_exposure(portfolio);
        let per_leg_gamma = self.gamma_exposure(portfolio);
        let portfolio_delta = per_leg_delta.values().sum();
        let total_gamma = per_leg_gamma.values().sum();
        RiskMetricsReport {
            per_leg_delta,
            portfolio_delta,
            per_leg_gamma,
            total_gamma,
            vega: 0.0,
            sharpe_with_legs: sharpe_ratio,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::{Side, Trade};
    use axon_core::portfolio::Portfolio;
    use axon_core::portfolio::currency::Currency;
    use axon_core::time::Timestamp;
    use axon_core::types::{
        Instrument, Price, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol,
    };

    fn btc_spot() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn btc_perp() -> Instrument {
        Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        })
    }

    fn make_trade(id: u64, _inst: &Instrument, side: Side, price: f64, qty: f64) -> Trade {
        let (buyer, seller) = match side {
            Side::Buy => (id, id + 1000),
            Side::Sell => (id + 1000, id),
        };
        Trade::new(
            Timestamp::from_nanos(id as i64 * 1_000_000),
            Price::from_f64(price),
            Quantity::from_f64(qty),
            buyer,
            seller,
        )
    }

    /// 工具:应用 trade 到 portfolio(taker_side 与 taker 方向一致时,加仓)
    fn apply_trade(portfolio: &mut Portfolio, inst: &Instrument, side: Side, price: f64, qty: f64) {
        let trade = make_trade(1, inst, side, price, qty);
        portfolio
            .apply_trade_instrument(inst, &trade, side, Timestamp::from_nanos(1_000_000))
            .expect("apply_trade ok");
    }

    // ─── 空 portfolio ──────────────────────────────

    #[test]
    fn empty_portfolio_zero_delta() {
        let engine = PortfolioRiskEngine::new();
        let portfolio = Portfolio::new(Currency::USDT, 0.0);
        assert_eq!(engine.portfolio_delta(&portfolio), 0.0);
        assert_eq!(engine.total_gamma(&portfolio), 0.0);
        assert_eq!(engine.vega(&portfolio), 0.0);
        assert!(engine.delta_exposure(&portfolio).is_empty());
    }

    // ─── 单 leg ───────────────────────────────────

    #[test]
    fn single_leg_long_1_btc() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Buy, 100.0, 1.0);
        let delta = engine.delta_exposure(&portfolio);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[&inst], 1.0, "1 BTC long → delta = +1");
        assert_eq!(engine.portfolio_delta(&portfolio), 1.0);
    }

    #[test]
    fn single_leg_short_2_btc() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Sell, 100.0, 2.0);
        assert_eq!(engine.portfolio_delta(&portfolio), -2.0);
    }

    // ─── 多 leg delta-neutral ─────────────────────

    #[test]
    fn spot_perp_delta_neutral_zero_total() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        let perp = btc_perp();
        // spot +1 BTC
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 1.0);
        // perp -1 BTC(对冲)
        apply_trade(&mut portfolio, &perp, Side::Sell, 100.5, 1.0);
        assert!(
            (engine.portfolio_delta(&portfolio) - 0.0).abs() < 1e-9,
            "delta-neutral: portfolio_delta = 0"
        );
    }

    // ─── multi leg 加减 ───────────────────────────

    #[test]
    fn multi_leg_aggregation() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        let perp = btc_perp();
        // spot +1 BTC, perp -0.5 BTC
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 1.0);
        apply_trade(&mut portfolio, &perp, Side::Sell, 100.5, 0.5);
        // 净 delta = 1 + (-0.5) = 0.5
        assert!((engine.portfolio_delta(&portfolio) - 0.5).abs() < 1e-9);
    }

    // ─── gamma 暂时全 0 ───────────────────────────

    #[test]
    fn gamma_is_zero_for_now() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let inst = btc_spot();
        apply_trade(&mut portfolio, &inst, Side::Buy, 100.0, 1.0);
        assert_eq!(engine.total_gamma(&portfolio), 0.0);
        let gamma = engine.gamma_exposure(&portfolio);
        assert_eq!(gamma[&inst], 0.0);
    }

    // ─── RiskMetricsReport aggregate ───────────────

    #[test]
    fn report_aggregate_computes_portfolio_delta() {
        let mut per_leg = HashMap::new();
        per_leg.insert(btc_spot(), 1.0);
        per_leg.insert(btc_perp(), -0.5);
        let report = RiskMetricsReport::aggregate(per_leg, 1.5);
        assert_eq!(report.portfolio_delta, 0.5);
        assert_eq!(report.sharpe_with_legs, 1.5);
        // gamma 全 0
        assert_eq!(report.total_gamma, 0.0);
        assert_eq!(report.vega, 0.0);
    }

    // ─── compute_report 完整路径 ───────────────────

    #[test]
    fn compute_report_full_path() {
        let engine = PortfolioRiskEngine::new();
        let mut portfolio = Portfolio::new(Currency::USDT, 0.0);
        let spot = btc_spot();
        apply_trade(&mut portfolio, &spot, Side::Buy, 100.0, 2.0);
        let report = engine.compute_report(&portfolio, 0.8);
        assert_eq!(report.per_leg_delta.len(), 1);
        assert_eq!(report.per_leg_delta[&spot], 2.0);
        assert_eq!(report.portfolio_delta, 2.0);
        assert_eq!(report.sharpe_with_legs, 0.8);
    }
}
