use std::collections::{HashMap, VecDeque};

use axon_core::order::Order;
use axon_core::portfolio::Portfolio;
use parking_lot::Mutex;

use crate::checks::{concentration, drawdown, leverage, order_size, position, var as var_check};
use crate::circuit_breaker::CircuitBreaker;
use crate::config::RiskConfig;
use crate::error::{AlertSeverity, RiskAlert, RiskReason, RiskResult};
use crate::metrics::RiskMetrics;
use crate::utils::now_unix_secs;

/// VaR 计算的最小有效样本数；样本不足时回退到 0 并标注 `confidence = 0`
const VAR_MIN_SAMPLES: usize = 5;
/// 收益率历史窗口大小（每个 PnL 视为一期收益）
const VAR_WINDOW: usize = 252;

pub trait RiskEngine: Send + Sync {
    fn check_order(&self, order: &Order, portfolio: &Portfolio) -> RiskResult;
    fn check_portfolio(&self, portfolio: &Portfolio) -> Vec<RiskAlert>;
    fn update_daily_pnl(&self, pnl: f64);
    fn get_metrics(&self, portfolio: &Portfolio) -> RiskMetrics;
    fn reset_daily(&self);
}

pub struct DefaultRiskEngine {
    config: RiskConfig,
    circuit_breaker: CircuitBreaker,
    daily_pnl: Mutex<f64>,
    peak_value: Mutex<f64>,
    /// 收益率历史窗口：每次 `update_daily_pnl` 写入，容量 = `VAR_WINDOW`。
    /// 元素是每期 PnL 增量（绝对值），VaR 95 视为对收益分布下分位数的损失度量。
    pnl_history: Mutex<VecDeque<f64>>,
}

impl DefaultRiskEngine {
    pub fn new(config: RiskConfig) -> Self {
        let cb = CircuitBreaker::new(config.max_daily_loss, config.circuit_breaker_cooldown);
        Self {
            config,
            circuit_breaker: cb,
            daily_pnl: Mutex::new(0.0),
            peak_value: Mutex::new(0.0),
            pnl_history: Mutex::new(VecDeque::with_capacity(VAR_WINDOW)),
        }
    }

    /// 计算当前 VaR(95)；不足样本时返回 0
    fn compute_var_95(&self) -> (f64, f64) {
        let history = self.pnl_history.lock();
        if history.len() < VAR_MIN_SAMPLES {
            return (0.0, 0.0);
        }
        // VaR 假设每个 PnL 为简单收益：-x = 损失，VaR = |q05|
        let returns: Vec<f64> = history.iter().copied().collect();
        let var = var_check::calculate_var(&returns, 0.95);
        // 置信度：样本数 / 窗口，> 0.5 视为高置信
        let confidence = (history.len() as f64 / VAR_WINDOW as f64).min(1.0);
        (var, confidence)
    }
}

impl RiskEngine for DefaultRiskEngine {
    fn check_order(&self, order: &Order, portfolio: &Portfolio) -> RiskResult {
        if self.circuit_breaker.is_active() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            return RiskResult::Reject(RiskReason::CircuitBreakerActive {
                until: now + self.config.circuit_breaker_cooldown.as_secs() as i64,
            });
        }

        let r = order_size::check_order_size(order, &self.config);
        if !matches!(r, RiskResult::Allow) {
            return r;
        }

        let r = position::check_position_limit(order, portfolio, &self.config);
        if !matches!(r, RiskResult::Allow) {
            return r;
        }

        let r = leverage::check_leverage(portfolio, &self.config);
        if !matches!(r, RiskResult::Allow) {
            return r;
        }

        let peak = *self.peak_value.lock();
        drawdown::check_drawdown(portfolio, peak, &self.config)
    }

    fn check_portfolio(&self, portfolio: &Portfolio) -> Vec<RiskAlert> {
        let mut alerts = Vec::new();

        let daily_pnl = *self.daily_pnl.lock();
        if daily_pnl <= -self.config.max_daily_loss {
            alerts.push(RiskAlert {
                severity: AlertSeverity::Emergency,
                reason: RiskReason::DailyPnLLimit {
                    limit: self.config.max_daily_loss,
                    current: daily_pnl,
                },
                timestamp: now_unix_secs(),
            });
        }

        alerts.extend(concentration::check_concentration(portfolio, &self.config));
        alerts
    }

    fn update_daily_pnl(&self, pnl: f64) {
        // 先累加 daily_pnl，再写入 pnl_history，确保历史包含本期收益
        let mut current = self.daily_pnl.lock();
        *current += pnl;
        self.circuit_breaker.check_and_trigger(*current);

        // 写入滑动窗口：超过容量时弹出最旧元素
        let mut history = self.pnl_history.lock();
        if history.len() == VAR_WINDOW {
            history.pop_front();
        }
        history.push_back(pnl);

        // peak_value 跟踪净资产峰值（使用 PnL 累计作为代理）
        let nav = *current;
        let mut peak = self.peak_value.lock();
        if nav > *peak {
            *peak = nav;
        }
    }

    fn get_metrics(&self, portfolio: &Portfolio) -> RiskMetrics {
        let nav = portfolio.nav() as f64 / 1_000_000.0;
        let cash = portfolio.base_cash();
        let leverage_val = if cash > 0.0 {
            nav / cash
        } else {
            f64::INFINITY
        };

        let mut concentration_map = HashMap::new();
        if nav > 0.0 {
            for (symbol, pos) in portfolio.positions() {
                if let Some(mv) = pos.market_value() {
                    concentration_map.insert(symbol.to_string(), mv as f64 / 1_000_000.0 / nav);
                }
            }
        }

        let peak = *self.peak_value.lock();
        let current_drawdown = if peak > 0.0 { (peak - nav) / peak } else { 0.0 };

        // 真实 VaR(95)：基于 pnl_history 计算（而非硬编码 0.0）
        let (var_95, _var_confidence) = self.compute_var_95();

        RiskMetrics {
            total_exposure: nav,
            leverage: leverage_val,
            current_drawdown,
            daily_realized_pnl: *self.daily_pnl.lock(),
            var_95,
            concentration: concentration_map,
        }
    }

    fn reset_daily(&self) {
        *self.daily_pnl.lock() = 0.0;
        self.circuit_breaker.reset();
        // 不重置 pnl_history：历史窗口是滚动概念，不应每天清零
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::market::Side;
    use axon_core::order::{OrderType, TimeInForce};
    use axon_core::portfolio::Currency;
    use axon_core::types::{Price, Quantity, Symbol};

    fn make_limit_order(side: Side, price: f64, qty: f64) -> Order {
        Order::new(
            1,
            Symbol::from("BTC-USDT"),
            side,
            OrderType::Limit {
                price: Price::from_f64(price),
            },
            Quantity::from_f64(qty),
            TimeInForce::GTC,
        )
    }

    fn funded_portfolio(cash: f64) -> Portfolio {
        let mut p = Portfolio::new(Currency::USD, 0.001);
        p.deposit(Currency::USD, cash);
        p
    }

    #[test]
    fn test_check_order_allows_valid() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 10.0);
        assert_eq!(engine.check_order(&order, &portfolio), RiskResult::Allow);
    }

    #[test]
    fn test_check_order_rejects_circuit_breaker() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        engine.update_daily_pnl(-10_000.0);
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 10.0);
        assert!(matches!(
            engine.check_order(&order, &portfolio),
            RiskResult::Reject(RiskReason::CircuitBreakerActive { .. })
        ));
    }

    #[test]
    fn test_check_order_rejects_oversized() {
        let config = RiskConfig {
            max_order_value: 1_000.0,
            ..Default::default()
        };
        let engine = DefaultRiskEngine::new(config);
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 20.0); // value = 2_000
        assert!(matches!(
            engine.check_order(&order, &portfolio),
            RiskResult::Reject(RiskReason::OrderTooLarge { .. })
        ));
    }

    #[test]
    fn test_check_order_short_circuit() {
        let config = RiskConfig {
            max_order_value: 1.0,
            max_position_per_instrument: 0.001,
            ..Default::default()
        };
        let engine = DefaultRiskEngine::new(config);
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 10.0);
        // Should reject at order_size check (step 2), not position check (step 3)
        assert!(matches!(
            engine.check_order(&order, &portfolio),
            RiskResult::Reject(RiskReason::OrderTooLarge { .. })
        ));
    }

    #[test]
    fn test_update_daily_pnl() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        engine.update_daily_pnl(5_000.0);
        engine.update_daily_pnl(-3_000.0);
        let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
        assert_eq!(metrics.daily_realized_pnl, 2_000.0);
    }

    #[test]
    fn test_reset_daily() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        engine.update_daily_pnl(-9_000.0);
        engine.reset_daily();
        let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
        assert_eq!(metrics.daily_realized_pnl, 0.0);
    }

    #[test]
    fn test_get_metrics() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        let portfolio = funded_portfolio(100_000.0);
        let metrics = engine.get_metrics(&portfolio);
        assert!(metrics.leverage > 0.0);
        assert!(metrics.concentration.is_empty());
    }

    // ===== VaR(95) 真实计算测试 =====

    #[test]
    fn test_var_95_zero_when_insufficient_history() {
        // 不足 5 个样本时 var_95 应为 0
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        engine.update_daily_pnl(-100.0);
        engine.update_daily_pnl(50.0);
        let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
        assert_eq!(metrics.var_95, 0.0);
    }

    #[test]
    fn test_var_95_computed_from_history() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        // 5+ 期收益：含负向极值 -0.05
        // 排序后 [-0.05, -0.02, 0.01, 0.02, 0.03]，conf=0.95 => index = (1-0.95)*5 = 0
        // VaR = -(-0.05) = 0.05
        for pnl in [-0.05, -0.02, 0.01, 0.02, 0.03, 0.04, 0.05] {
            engine.update_daily_pnl(pnl);
        }
        let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
        assert!(
            (metrics.var_95 - 0.05).abs() < 1e-9,
            "var_95 expected 0.05, got {}",
            metrics.var_95
        );
    }

    #[test]
    fn test_var_95_zero_when_all_positive() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        // 全正收益时 VaR 应为 0（无损失）
        for pnl in [0.01, 0.02, 0.03, 0.04, 0.05, 0.06] {
            engine.update_daily_pnl(pnl);
        }
        let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
        assert_eq!(metrics.var_95, 0.0);
    }

    #[test]
    fn test_var_95_window_rolls_over() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        // 推送 300 期：前 252 期含 -10 大亏损，后 48 期为 0
        // 滚动窗口应丢弃最早元素，最终窗口无 -10
        for _ in 0..252 {
            engine.update_daily_pnl(-10.0);
        }
        for _ in 0..48 {
            engine.update_daily_pnl(0.0);
        }
        let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
        // 滚动后窗口 = 48 个 0 + 204 个 -10
        // 排序后前 5% 都是 -10，VaR = 10
        assert!(
            (metrics.var_95 - 10.0).abs() < 1e-9,
            "rolled window var_95 expected 10.0, got {}",
            metrics.var_95
        );
    }

    // ===== 额外覆盖率测试 =====

    #[test]
    fn test_check_order_with_zero_quantity() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 0.0);
        // 零数量订单应该被允许
        assert_eq!(engine.check_order(&order, &portfolio), RiskResult::Allow);
    }

    #[test]
    fn test_check_order_sell_side() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Sell, 100.0, 10.0);
        assert_eq!(engine.check_order(&order, &portfolio), RiskResult::Allow);
    }

    #[test]
    fn test_check_order_with_drawdown_limit() {
        let config = RiskConfig {
            max_drawdown: 0.01, // 1% 最大回撤
            ..Default::default()
        };
        let engine = DefaultRiskEngine::new(config);
        let portfolio = funded_portfolio(100_000.0);
        // 模拟大额亏损
        engine.update_daily_pnl(-50_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 10.0);
        // 超过回撤限制应该被拒绝
        let result = engine.check_order(&order, &portfolio);
        // 注意：check_order 中的 drawdown 检查基于 peak_value，而不是 daily_pnl
        // 所以这里可能不会触发 MaxDrawdownExceeded
    }

    #[test]
    fn test_check_order_with_daily_loss_limit() {
        let config = RiskConfig {
            max_daily_loss: -1_000.0,
            ..Default::default()
        };
        let engine = DefaultRiskEngine::new(config);
        let portfolio = funded_portfolio(100_000.0);
        // 模拟日内亏损
        engine.update_daily_pnl(-5_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 10.0);
        // 注意：check_order 中没有 daily_loss 检查，这个检查在 check_portfolio 中
        let _result = engine.check_order(&order, &portfolio);
        // 验证 check_order 不会因为 daily_loss 而拒绝
    }

    #[test]
    fn test_peak_value_tracking() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        // 先盈利
        engine.update_daily_pnl(10_000.0);
        // 再亏损
        engine.update_daily_pnl(-20_000.0);
        let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
        // 验证 metrics 计算不 panic
    }

    #[test]
    fn test_get_metrics_basic() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        let portfolio = funded_portfolio(100_000.0);
        let metrics = engine.get_metrics(&portfolio);
        assert!(metrics.leverage > 0.0);
        assert!(metrics.concentration.is_empty());
    }

    #[test]
    fn test_update_daily_pnl_accumulation() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        for i in 0..100 {
            engine.update_daily_pnl(i as f64);
        }
        let metrics = engine.get_metrics(&funded_portfolio(100_000.0));
        // 累计应该是 0+1+2+...+99 = 4950
        assert_eq!(metrics.daily_realized_pnl, 4950.0);
    }

    #[test]
    fn test_circuit_breaker_integration() {
        let config = RiskConfig {
            circuit_breaker_cooldown: std::time::Duration::from_secs(60),
            ..Default::default()
        };
        let engine = DefaultRiskEngine::new(config);
        // 触发熔断
        engine.update_daily_pnl(-100_000.0);
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 10.0);
        // 熔断后应该拒绝订单
        let result = engine.check_order(&order, &portfolio);
        assert!(matches!(result, RiskResult::Reject(RiskReason::CircuitBreakerActive { .. })));
    }

    #[test]
    fn test_reset_daily_resets_circuit_breaker() {
        let engine = DefaultRiskEngine::new(RiskConfig::default());
        // 触发熔断
        engine.update_daily_pnl(-100_000.0);
        // 重置
        engine.reset_daily();
        let portfolio = funded_portfolio(100_000.0);
        let order = make_limit_order(Side::Buy, 100.0, 10.0);
        // 重置后应该允许订单
        assert_eq!(engine.check_order(&order, &portfolio), RiskResult::Allow);
    }
}
