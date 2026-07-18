//! 0.6.0 新增:跨 leg 对冲对(`LegPair`)风险约束
//!
//! 提供三个 API:
//!
//! - [`leg_pair_net_exposure`]:计算 `spot_qty + perp_qty * hedge_ratio`(delta-neutral 检查)
//! - [`check_leg_pair_net_exposure`]:净暴露超限时返回 `RiskResult::Reject`
//! - [`per_leg_var`]:对单一 leg 的历史 returns 序列计算 VaR(95% 置信)

use axon_core::portfolio::Portfolio;
use axon_core::types::LegPair;

use crate::error::{RiskReason, RiskResult};

/// 计算 `LegPair` 当前净暴露
///
/// 公式:`net = spot_qty + perp_qty * hedge_ratio`
/// - 理想 delta 中性时 = 0
/// - 正值:多 spot 净敞口(perp 对冲不足)
/// - 负值:空 spot 净敞口(perp 对冲过多)
pub fn leg_pair_net_exposure(portfolio: &Portfolio, pair: &LegPair) -> f64 {
    let spot_qty = portfolio
        .positions()
        .get(&pair.spot)
        .map(|p| p.quantity.as_f64())
        .unwrap_or(0.0);
    let perp_qty = portfolio
        .positions()
        .get(&pair.perp)
        .map(|p| p.quantity.as_f64())
        .unwrap_or(0.0);
    spot_qty + perp_qty * pair.hedge_ratio
}

/// 净暴露超限时返回 Reject
///
/// `max_abs` 含义:`|net|` 上限。delta 中性策略默认 `0.0`;允许小偏离时给一个
/// 浮点容差,如 `1e-6`。
pub fn check_leg_pair_net_exposure(
    portfolio: &Portfolio,
    pair: &LegPair,
    max_abs: f64,
) -> RiskResult {
    let net = leg_pair_net_exposure(portfolio, pair);
    if net.abs() > max_abs {
        return RiskResult::Reject(RiskReason::LegPairNetExposureExceeded {
            pair: pair.label(),
            current: net,
            limit: max_abs,
        });
    }
    RiskResult::Allow
}

/// 单 leg 的历史收益 VaR
///
/// # 参数
///
/// - `returns`:该 leg 的历史对数收益 / 简单收益序列(任意分布,长度建议 ≥ 30)
/// - `confidence`:置信度,默认 0.95
///
/// # 返回
///
/// 正数 = 该 leg 在 `confidence` 置信下的最大单期损失(绝对值,USD 单位为调用方负责)。
pub fn per_leg_var(returns: &[f64], confidence: f64) -> f64 {
    // 复用 checks::var::calculate_var(历史模拟法,排序后取 `(1 - confidence)` 分位点)
    crate::checks::var::calculate_var(returns, confidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::portfolio::Position;
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

    fn make_position(instrument: Instrument, qty: f64) -> Position {
        Position {
            symbol: instrument.label().into(),
            instrument,
            quantity: Quantity::from_f64(qty),
            avg_cost: Price::from_f64(50_000.0),
            market_price: Some(Price::from_f64(50_000.0)),
            realized_pnl: 0,
            side: if qty >= 0.0 {
                axon_core::market::Side::Buy
            } else {
                axon_core::market::Side::Sell
            },
        }
    }

    #[test]
    fn test_net_exposure_delta_neutral() {
        let pair = LegPair::new(btc_spot(), btc_perp());
        let mut pf = Portfolio::default();
        pf.add_position(make_position(btc_spot(), 1.0));
        pf.add_position(make_position(btc_perp(), -1.0));
        let net = leg_pair_net_exposure(&pf, &pair);
        assert!(net.abs() < 1e-9, "delta 中性对 net 应为 0,got {net}");
    }

    #[test]
    fn test_net_exposure_long_bias() {
        let pair = LegPair::new(btc_spot(), btc_perp());
        let mut pf = Portfolio::default();
        pf.add_position(make_position(btc_spot(), 1.0));
        // 没有 perp → net = 1.0
        let net = leg_pair_net_exposure(&pf, &pair);
        assert!((net - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_check_leg_pair_net_exposure_reject() {
        let pair = LegPair::new(btc_spot(), btc_perp());
        let mut pf = Portfolio::default();
        pf.add_position(make_position(btc_spot(), 2.0));
        pf.add_position(make_position(btc_perp(), 0.0));
        // net = 2.0,超过 0.5 上限
        let result = check_leg_pair_net_exposure(&pf, &pair, 0.5);
        assert!(matches!(
            result,
            RiskResult::Reject(RiskReason::LegPairNetExposureExceeded { .. })
        ));
    }

    #[test]
    fn test_check_leg_pair_net_exposure_allow() {
        let pair = LegPair::new(btc_spot(), btc_perp());
        let mut pf = Portfolio::default();
        pf.add_position(make_position(btc_spot(), 1.0));
        pf.add_position(make_position(btc_perp(), -1.0));
        // net ≈ 0,严格上限 0.0 → Allow
        let result = check_leg_pair_net_exposure(&pf, &pair, 0.0);
        assert_eq!(result, RiskResult::Allow);
    }

    #[test]
    fn test_per_leg_var_basic() {
        let returns = vec![
            -0.05, -0.03, -0.01, 0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07,
        ];
        let var = per_leg_var(&returns, 0.95);
        assert!(var > 0.0, "VaR 应 > 0,got {var}");
    }

    #[test]
    fn test_per_leg_var_with_hedge_ratio() {
        // 0.5 hedge ratio:perp qty = 2.0 时 = spot 1.0
        let pair = LegPair::with_ratio(btc_spot(), btc_perp(), 0.5);
        let mut pf = Portfolio::default();
        pf.add_position(make_position(btc_spot(), 1.0));
        pf.add_position(make_position(btc_perp(), -2.0));
        let net = leg_pair_net_exposure(&pf, &pair);
        assert!(net.abs() < 1e-9);
    }
}
