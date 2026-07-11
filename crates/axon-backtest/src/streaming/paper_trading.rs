//! 模拟盘模式
//!
//! 0.4.0:PaperTradingEngine 强化 — `partial fill` 裁决生效,
//! `seed` 化 rng 让测试可重复。
//!
//! ## 裁决语义
//!
//! 每笔限价单提交时,先经 `should_fill()` 二元裁决:
//! - `fill_probability = 1.0` → 100% 整笔成交(走 fill_ratio = 1.0)
//! - `fill_probability = 0.0` → 100% 拒单(应不进入撮合)
//! - `0 < fill_probability < 1.0` → 依均匀分布 `rng.gen() < fill_probability` 决定全成/全拒
//!
//! 整笔成交后,`fill_ratio()` 进一步决定 partial 比例(0.5~1.0),
//! 0.4.0 MVP 阶段仅在 `fill_probability = 1.0` 时启用(避免双随机),
//! 给"全成订单的子集被打折"留扩展位。

use std::time::Duration;

use axon_core::market::Side;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// 模拟交易所配置
pub struct SimulatedExchange {
    /// 基础延迟
    ///
    /// 0.4.0:字段保留但**不生效**(`simulated_latency()` 仅 expose 读),留 0.4.0+1
    pub base_latency: Duration,
    /// 滑点(基点)
    pub slippage_bps: f64,
    /// 成交概率(0.0~1.0)— 控制 should_fill 二元裁决
    pub fill_probability: f64,
    /// 0.4.0:随机种子(`None` = 每次新建 `PaperTradingEngine` 时取系统时间)
    pub seed: Option<u64>,
    /// 0.4.0:partial fill 下界系数(`[partial_fill_min_ratio, 1.0]` 区间)
    ///
    /// - `1.0`(默认)= 总是 1.0(不缩量,旧行为)
    /// - `< 1.0` = 启用 partial fill 随机
    pub partial_fill_min_ratio: f64,
}

impl Default for SimulatedExchange {
    fn default() -> Self {
        Self {
            base_latency: Duration::from_millis(10),
            slippage_bps: 1.0,
            fill_probability: 0.95,
            seed: None,
            partial_fill_min_ratio: 1.0,
        }
    }
}

/// 模拟盘引擎
pub struct PaperTradingEngine {
    exchange: SimulatedExchange,
    /// 0.4.0:seed 化随机源
    rng: StdRng,
}

impl PaperTradingEngine {
    /// 创建新的模拟盘引擎
    ///
    /// `seed` 取自 `SimulatedExchange::seed`,`None` 时用确定性默认 seed(便于测试可重复)
    pub fn new(exchange: SimulatedExchange) -> Self {
        let seed = exchange.seed.unwrap_or(0xC0FFEE_u64);
        Self {
            exchange,
            rng: StdRng::seed_from_u64(seed),
        }
    }

    /// Seed 化(0.4.0 新增)— 测试用,确定 random 序列
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = StdRng::seed_from_u64(seed);
        self
    }

    /// 0.4.0:设置 fill_probability(0.0~1.0,clamp)— 测试用,确定 should_fill 行为
    pub fn with_fill_probability(mut self, p: f64) -> Self {
        self.exchange.fill_probability = p.clamp(0.0, 1.0);
        self
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

    /// 0.4.0:裁决是否成交(整笔)
    ///
    /// 语义:
    /// - `fill_probability = 1.0` → 总是 `true`
    /// - `fill_probability = 0.0` → 总是 `false`
    /// - 其他 → `rng.gen::<f64>() < fill_probability`
    pub fn should_fill(&mut self) -> bool {
        let p = self.exchange.fill_probability.clamp(0.0, 1.0);
        if p >= 1.0 {
            return true;
        }
        if p <= 0.0 {
            return false;
        }
        self.rng.gen_range(0.0..1.0) < p
    }

    /// 0.4.0:成交比例系数(`[partial_fill_min_ratio, 1.0]` 区间)
    ///
    /// - `partial_fill_min_ratio = 1.0`(默认)= 总是 1.0(不缩量,旧行为)
    /// - `< 1.0` = 启用 partial fill 随机(`[min, 1.0]` 之间均匀分布)
    pub fn fill_ratio(&mut self) -> f64 {
        let min_r = self.exchange.partial_fill_min_ratio;
        if min_r >= 1.0 {
            return 1.0;
        }
        // [min_r, 1.0] 之间均匀分布
        min_r + self.rng.gen_range(0.0..1.0) * (1.0 - min_r)
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

    #[test]
    fn test_should_fill_with_full_probability() {
        let ex = SimulatedExchange {
            fill_probability: 1.0,
            ..SimulatedExchange::default()
        };
        let mut engine = PaperTradingEngine::new(ex);
        for _ in 0..100 {
            assert!(engine.should_fill());
        }
    }

    #[test]
    fn test_should_fill_with_zero_probability() {
        let ex = SimulatedExchange {
            fill_probability: 0.0,
            ..SimulatedExchange::default()
        };
        let mut engine = PaperTradingEngine::new(ex);
        for _ in 0..100 {
            assert!(!engine.should_fill());
        }
    }

    #[test]
    fn test_should_fill_partial_probability_distribution() {
        let ex = SimulatedExchange {
            fill_probability: 0.5,
            ..SimulatedExchange::default()
        };
        let mut engine = PaperTradingEngine::new(ex).with_seed(42);
        let n = 10_000;
        let yes = (0..n).filter(|_| engine.should_fill()).count();
        let ratio = yes as f64 / n as f64;
        // 0.5 ± 0.05 内
        assert!(
            (0.45..=0.55).contains(&ratio),
            "fill_probability=0.5 应均匀分布,实为 {ratio}"
        );
    }

    #[test]
    fn test_fill_ratio_default_is_one() {
        // 默认 partial_fill_min_ratio = 1.0 → fill_ratio 总返 1.0(不缩量)
        let mut engine = PaperTradingEngine::new(SimulatedExchange::default());
        for _ in 0..100 {
            assert!((engine.fill_ratio() - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn test_fill_ratio_partial_in_min_to_1_range() {
        // partial_fill_min_ratio = 0.5 → fill_ratio 在 [0.5, 1.0] 均匀分布
        let mut engine = PaperTradingEngine::new(SimulatedExchange {
            partial_fill_min_ratio: 0.5,
            ..SimulatedExchange::default()
        })
        .with_seed(123);
        for _ in 0..100 {
            let r = engine.fill_ratio();
            assert!((0.5..=1.0).contains(&r), "r={r}");
        }
    }

    #[test]
    fn test_with_seed_deterministic() {
        let mut a = PaperTradingEngine::new(SimulatedExchange {
            fill_probability: 0.5,
            ..SimulatedExchange::default()
        })
        .with_seed(999);
        let mut b = PaperTradingEngine::new(SimulatedExchange {
            fill_probability: 0.5,
            ..SimulatedExchange::default()
        })
        .with_seed(999);
        for _ in 0..100 {
            assert_eq!(a.should_fill(), b.should_fill());
        }
    }
}
