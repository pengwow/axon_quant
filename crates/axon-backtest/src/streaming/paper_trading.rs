//! 模拟盘模式

use std::time::Duration;

use axon_core::market::Side;

/// 模拟交易所配置
pub struct SimulatedExchange {
    /// 基础延迟
    pub base_latency: Duration,
    /// 滑点（基点）
    pub slippage_bps: f64,
    /// 成交概率
    pub fill_probability: f64,
}

impl Default for SimulatedExchange {
    fn default() -> Self {
        Self {
            base_latency: Duration::from_millis(10),
            slippage_bps: 1.0,
            fill_probability: 0.95,
        }
    }
}

/// 模拟盘引擎
pub struct PaperTradingEngine {
    exchange: SimulatedExchange,
}

impl PaperTradingEngine {
    /// 创建新的模拟盘引擎
    pub fn new(exchange: SimulatedExchange) -> Self {
        Self { exchange }
    }

    /// 获取模拟延迟
    pub fn simulated_latency(&self) -> Duration {
        self.exchange.base_latency
    }

    /// 计算滑点后的价格
    pub fn apply_slippage(&self, price: f64, side: Side) -> f64 {
        let slippage = price * self.exchange.slippage_bps / 10000.0;
        match side {
            Side::Buy => price + slippage,
            Side::Sell => price - slippage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simulated_exchange_default() {
        let exchange = SimulatedExchange::default();
        assert_eq!(exchange.base_latency, Duration::from_millis(10));
        assert_eq!(exchange.slippage_bps, 1.0);
    }

    #[test]
    fn test_apply_slippage_buy() {
        let engine = PaperTradingEngine::new(SimulatedExchange::default());
        let price = engine.apply_slippage(100.0, Side::Buy);
        assert!((price - 100.01).abs() < 1e-6);
    }

    #[test]
    fn test_apply_slippage_sell() {
        let engine = PaperTradingEngine::new(SimulatedExchange::default());
        let price = engine.apply_slippage(100.0, Side::Sell);
        assert!((price - 99.99).abs() < 1e-6);
    }
}
