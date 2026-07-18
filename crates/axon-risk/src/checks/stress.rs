//! 0.6.0 新增:压力测试 — 价格冲击下计算组合的假设 NAV 影响
//!
//! 提供三个 API:
//!
//! - [`stress_leg`]:单 leg 在价格冲击下的 PnL 影响
//! - [`stress_pair`]:跨 leg 对冲对的合计 PnL 影响(零和特性 → 衡量残余风险)
//! - [`stress_portfolio`]:多 leg 组合在统一价格冲击下的合计 PnL

use axon_core::portfolio::Portfolio;
use axon_core::types::LegPair;

/// 单 leg 在给定价格冲击(`shock_pct` 表示价格变动比例,正=上涨,负=下跌)下的 PnL 影响
///
/// 公式:`impact = quantity * market_price * shock_pct`
///
/// # 注意
///
/// - `quantity` 符号表示方向(正=多头),所以多头在正冲击时 = 盈利(正值)
/// - 若 `market_price` 为 `None`(无最新价),返回 `0.0`
pub fn stress_leg(quantity: f64, market_price: Option<f64>, shock_pct: f64) -> f64 {
    let price = match market_price {
        Some(p) => p,
        None => return 0.0,
    };
    quantity * price * shock_pct
}

/// 跨 leg 对冲对的合计 PnL 影响(用于评估"delta 中性"在冲击下的残余风险)
///
/// 对 spot 和 perp 同等应用 `shock_pct`(假设二者价格完全相关,极端情形),
/// 求和 → 理想对冲时 = 0,有偏差时 ≠ 0(衡量基差风险)。
pub fn stress_pair(portfolio: &Portfolio, pair: &LegPair, shock_pct: f64) -> f64 {
    let spot = portfolio.position_by_instrument(&pair.spot);
    let perp = portfolio.position_by_instrument(&pair.perp);

    let spot_impact = spot
        .map(|p| {
            stress_leg(
                p.quantity.as_f64(),
                p.market_price.map(|m| m.as_f64()),
                shock_pct,
            )
        })
        .unwrap_or(0.0);
    let perp_impact = perp
        .map(|p| {
            stress_leg(
                p.quantity.as_f64(),
                p.market_price.map(|m| m.as_f64()),
                shock_pct,
            )
        })
        .unwrap_or(0.0);
    spot_impact + perp_impact
}

/// 多 leg 组合在统一价格冲击下的合计 PnL
///
/// 遍历所有 position,对每个独立施加 `shock_pct`,求和。
pub fn stress_portfolio(portfolio: &Portfolio, shock_pct: f64) -> f64 {
    portfolio
        .positions()
        .values()
        .map(|p| {
            stress_leg(
                p.quantity.as_f64(),
                p.market_price.map(|m| m.as_f64()),
                shock_pct,
            )
        })
        .sum()
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

    fn make_position(instrument: Instrument, qty: f64, price: f64) -> Position {
        Position {
            symbol: instrument.label().into(),
            instrument,
            quantity: Quantity::from_f64(qty),
            avg_cost: Price::from_f64(price),
            market_price: Some(Price::from_f64(price)),
            realized_pnl: 0,
            side: if qty >= 0.0 {
                axon_core::market::Side::Buy
            } else {
                axon_core::market::Side::Sell
            },
        }
    }

    #[test]
    fn test_stress_leg_long_positive_shock() {
        // 多 1 BTC @ 50_000,+5% → +2_500
        let impact = stress_leg(1.0, Some(50_000.0), 0.05);
        assert!((impact - 2_500.0).abs() < 1e-6);
    }

    #[test]
    fn test_stress_leg_short_negative_shock() {
        // 空 1 BTC @ 50_000,+5% → -2_500
        let impact = stress_leg(-1.0, Some(50_000.0), 0.05);
        assert!((impact - (-2_500.0)).abs() < 1e-6);
    }

    #[test]
    fn test_stress_leg_no_market_price() {
        let impact = stress_leg(1.0, None, 0.05);
        assert_eq!(impact, 0.0);
    }

    #[test]
    fn test_stress_pair_delta_neutral_zero_sum() {
        // spot +1, perp -1,价格完全相关 → 冲击 PnL 应为 0
        let pair = LegPair::new(btc_spot(), btc_perp());
        let mut pf = Portfolio::default();
        pf.add_position(make_position(btc_spot(), 1.0, 50_000.0));
        pf.add_position(make_position(btc_perp(), -1.0, 50_000.0));
        let impact = stress_pair(&pf, &pair, 0.05);
        assert!(impact.abs() < 1e-6, "delta 中性对 PnL 应为 0,got {impact}");
    }

    #[test]
    fn test_stress_pair_unhedged_equals_long() {
        // 只有 spot 多头,无 perp → 冲击 PnL = 多头 spot 损益
        let pair = LegPair::new(btc_spot(), btc_perp());
        let mut pf = Portfolio::default();
        pf.add_position(make_position(btc_spot(), 1.0, 50_000.0));
        let impact = stress_pair(&pf, &pair, 0.05);
        assert!((impact - 2_500.0).abs() < 1e-6);
    }

    #[test]
    fn test_stress_portfolio_multi_leg() {
        // spot +1, perp -1, 额外的 leg +0.5
        let eth = Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        });
        let mut pf = Portfolio::default();
        pf.add_position(make_position(btc_spot(), 1.0, 50_000.0));
        pf.add_position(make_position(btc_perp(), -1.0, 50_000.0));
        pf.add_position(make_position(eth, 0.5, 3_000.0));
        // spot+perp 抵消,ETH +0.5 @ 3000 × 0.05 = 75
        let impact = stress_portfolio(&pf, 0.05);
        assert!((impact - 75.0).abs() < 1e-6, "got {impact}");
    }
}
