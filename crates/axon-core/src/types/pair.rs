//! 0.6.0 新增:LegPair — 跨 leg 配对抽象
//!
//! 表示"同标的 spot + perp 对冲对",用于:
//! - 跨 leg 风险约束(perp 净敞口 = spot qty - perp qty × hedge_ratio)
//! - 跨 leg VaR 计算(covariance + 个体 VaR 聚合)
//! - `CrossPair` 薄包装(`axon-backtest` 的 L3 撮合概念复用 `LegPair`)
//!
//! `LegPair` 放在 `axon-core` 是为了:
//! - `axon-risk` 算净敞口时不依赖 `axon-backtest`(避免反向依赖)
//! - `axon-backtest::CrossPair` 仅作为 L3 撮合的"扩展视图"层
//! - `axon-oms` 持久化 spot+perp 对时也直接复用

use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use super::instrument::Instrument;

/// Spot + perp 对冲对
///
/// # 0.6.0 新增
///
/// 替代 `axon-backtest::matching::l3::CrossPair` 的 spot/perp 部分。
/// `hedge_ratio` 通常 1.0(BTC spot + BTC perp 1:1 对冲),但允许调整
/// (e.g. ETH/BTC pair 0.06 ratio + perp ETH 1:1)。
///
/// `axon-risk` 通过 `LegPair` 算净敞口,不直接依赖 `axon-backtest`。
///
/// `Hash` / `Eq` 手动实现:`hedge_ratio: f64` 不可派生 `Hash` / `Eq`(`f64` 含 NaN)。
/// 我们对 `f64` 用 `to_bits()` 转成 `u64` 后再比较和 hash,语义上"位级相等即相等",
/// NaN 与 NaN 比较也会相等(因为位相同),这在 HashMap key 场景下是合理选择。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegPair {
    /// 现货 leg
    pub spot: Instrument,
    /// 永续合约 leg
    pub perp: Instrument,
    /// 对冲比率(perp qty = spot qty × hedge_ratio 时为 delta 中性)
    pub hedge_ratio: f64,
}

impl Eq for LegPair {}

impl Hash for LegPair {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.spot.hash(state);
        self.perp.hash(state);
        self.hedge_ratio.to_bits().hash(state);
    }
}

impl LegPair {
    /// 构造 spot+perp 1:1 对冲对(常用)
    pub fn new(spot: Instrument, perp: Instrument) -> Self {
        Self {
            spot,
            perp,
            hedge_ratio: 1.0,
        }
    }

    /// 构造带自定义 `hedge_ratio` 的对冲对
    pub fn with_ratio(spot: Instrument, perp: Instrument, hedge_ratio: f64) -> Self {
        Self {
            spot,
            perp,
            hedge_ratio,
        }
    }

    /// 校验 `spot` 确实是 spot,`perp` 确实是 swap(否则不是合法的对冲对)
    ///
    /// 用于 OMS 提交 / 风险检查时防止 spot-spot 或 perp-perp 错配。
    pub fn is_valid(&self) -> bool {
        matches!(self.spot, Instrument::Spot(_)) && matches!(self.perp, Instrument::Swap(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SpotInstrument, SwapInstrument, SwapSettle, Symbol};

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

    fn eth_spot() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        })
    }

    #[test]
    fn test_leg_pair_new_default_ratio() {
        let pair = LegPair::new(btc_spot(), btc_perp());
        assert_eq!(pair.spot, btc_spot());
        assert_eq!(pair.perp, btc_perp());
        assert!((pair.hedge_ratio - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_leg_pair_with_ratio() {
        let pair = LegPair::with_ratio(btc_spot(), btc_perp(), 0.5);
        assert!((pair.hedge_ratio - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_leg_pair_valid() {
        // 正常 spot+perp 配对 → 合法
        assert!(LegPair::new(btc_spot(), btc_perp()).is_valid());
        // spot-spot 配对 → 不合法
        assert!(!LegPair::new(btc_spot(), eth_spot()).is_valid());
    }

    #[test]
    fn test_leg_pair_serde() {
        let pair = LegPair::with_ratio(btc_spot(), btc_perp(), 1.5);
        let json = serde_json::to_string(&pair).unwrap();
        let parsed: LegPair = serde_json::from_str(&json).unwrap();
        assert_eq!(pair, parsed);
    }
}
