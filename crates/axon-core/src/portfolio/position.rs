//! 单资产持仓

use serde::{Deserialize, Serialize};

use crate::market::Side;
use crate::types::{Instrument, Price, Quantity, Symbol};

/// 单资产持仓
///
/// 数量符号表示方向:正数=多头,负数=空头。
///
/// # 0.5.0 字段扩展
///
/// 增加 `instrument: Instrument` 字段以支持多 leg 回测(spot + perp)区分。
/// 在 0.5.0 之前,spot 和 perp 共享同一 `Symbol`(如 `"BTC/USDT"`),
/// 会导致 `HashMap<Symbol, _>` key 碰撞,把 delta-neutral 双 leg 净持仓错算为 0。
/// `instrument` 字段提供 spot/perp 区分,使 risk engine / multi-leg backtest 能正确处理。
///
/// 与 `symbol` 字段关系:`symbol` 保留为人类可读 label(`"BTC/USDT"`),
/// `instrument` 是结构化区分(`Spot` / `Swap` variant)。两者并存,调用方可任选。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// 标的代码(人类可读 label,如 `"BTC/USDT"`,保留以兼容旧 API)
    pub symbol: Symbol,
    /// 0.5.0 新增:品种结构化区分(Spot / Swap),用于消除 spot+perp key 碰撞
    pub instrument: Instrument,
    /// 持仓数量(正数=多头,负数=空头)
    pub quantity: Quantity,
    /// 加权平均成本
    pub avg_cost: Price,
    /// 最新市场价格(用于未实现盈亏计算)
    pub market_price: Option<Price>,
    /// 已实现盈亏累计(单位:f64 × 1e6)
    pub realized_pnl: i64,
    /// 持仓方向(数量为 0 时默认为 Buy)
    pub side: Side,
}

impl Position {
    /// 创建新持仓(从 symbol 派生 instrument,0.5.0 兼容 API)
    ///
    /// `instrument` 由 `symbol` 通过 `Instrument::default_for(symbol)` 派生(空 Instrument),
    /// 保留以兼容旧调用方;**新代码**请用 [`Position::with_instrument`] 显式指定 instrument。
    pub fn new(symbol: Symbol, quantity: Quantity, avg_cost: Price) -> Self {
        let side = if quantity.as_f64() >= 0.0 {
            Side::Buy
        } else {
            Side::Sell
        };
        // 0.5.0 兼容:从 symbol 派生 instrument(空 Instrument,标记为"未指定品种")。
        // 0.5.0 之前只有 Symbol 没有 Instrument,这里给一个"无 Instrument"的占位值,
        // 不破坏旧调用方;新代码用 `with_instrument` 显式注入。
        Self {
            symbol,
            instrument: Instrument::default(),
            quantity,
            avg_cost,
            market_price: None,
            realized_pnl: 0,
            side,
        }
    }

    /// 0.5.0 新增:从 instrument 创建持仓(推荐用法)
    ///
    /// 与 [`Position::new`] 的区别:用 `instrument` 作为结构化区分,
    /// 避免 spot/perp key 碰撞;`symbol` 字段从 `instrument` 派生(`"{base}/{quote}"`)。
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use axon_core::portfolio::Position;
    /// use axon_core::types::{Instrument, SpotInstrument, SwapInstrument, SwapSettle, Symbol};
    /// use axon_core::market::Side;
    ///
    /// let spot = Position::with_instrument(
    ///     Instrument::Spot(SpotInstrument { base: Symbol::from("BTC"), quote: Symbol::from("USDT") }),
    ///     Quantity::from_f64(0.5),
    ///     Price::from_f64(50_000.0),
    /// );
    /// assert_eq!(spot.instrument.kind(), "spot");
    /// ```
    pub fn with_instrument(instrument: Instrument, quantity: Quantity, avg_cost: Price) -> Self {
        let symbol = Symbol::from(instrument.label());
        let side = if quantity.as_f64() >= 0.0 {
            Side::Buy
        } else {
            Side::Sell
        };
        Self {
            symbol,
            instrument,
            quantity,
            avg_cost,
            market_price: None,
            realized_pnl: 0,
            side,
        }
    }

    /// 0.5.0 新增:返回 instrument 引用(便捷访问)
    #[inline]
    pub fn instrument(&self) -> &Instrument {
        &self.instrument
    }

    /// 持仓市值（quantity × market_price）
    pub fn market_value(&self) -> Option<i64> {
        let mp = self.market_price?.as_f64();
        let v = self.quantity.as_f64() * mp;
        Some((v * 1_000_000.0) as i64)
    }

    /// 未实现盈亏（单位：f64 × 1e6）
    pub fn unrealized_pnl(&self) -> i64 {
        let mp = match self.market_price {
            Some(p) => p.as_f64(),
            None => return 0,
        };
        let qty = self.quantity.as_f64();
        let cost = self.avg_cost.as_f64();
        ((qty * (mp - cost)) * 1_000_000.0) as i64
    }

    /// 成本基础
    #[inline]
    pub fn cost_basis(&self) -> i64 {
        (self.quantity.as_f64().abs() * self.avg_cost.as_f64() * 1_000_000.0) as i64
    }

    /// 是否为空仓
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.quantity.as_f64().abs() < f64::EPSILON
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_long_position() {
        let p = Position::new(
            Symbol::from("BTC-USDT"),
            Quantity::from_f64(1.0),
            Price::from_f64(50_000.0),
        );
        assert_eq!(p.side, Side::Buy);
        assert!(!p.is_empty());
    }

    #[test]
    fn test_new_short_position() {
        let p = Position::new(
            Symbol::from("BTC-USDT"),
            Quantity::from_f64(-1.0),
            Price::from_f64(50_000.0),
        );
        assert_eq!(p.side, Side::Sell);
    }

    #[test]
    fn test_zero_quantity_is_empty() {
        let p = Position::new(
            Symbol::from("BTC-USDT"),
            Quantity::from_f64(0.0),
            Price::from_f64(0.0),
        );
        assert!(p.is_empty());
    }

    #[test]
    fn test_market_value() {
        let mut p = Position::new(
            Symbol::from("BTC-USDT"),
            Quantity::from_f64(1.0),
            Price::from_f64(50_000.0),
        );
        p.market_price = Some(Price::from_f64(55_000.0));
        let mv = p.market_value().unwrap();
        // 1.0 * 55000.0 = 55000.0
        assert!((mv - 55_000_000_000).abs() < 1_000_000);
    }

    #[test]
    fn test_market_value_no_market_price() {
        let p = Position::new(
            Symbol::from("BTC-USDT"),
            Quantity::from_f64(1.0),
            Price::from_f64(50_000.0),
        );
        assert!(p.market_value().is_none());
    }

    #[test]
    fn test_unrealized_pnl_long() {
        let mut p = Position::new(
            Symbol::from("BTC-USDT"),
            Quantity::from_f64(1.0),
            Price::from_f64(50_000.0),
        );
        p.market_price = Some(Price::from_f64(55_000.0));
        let upnl = p.unrealized_pnl();
        // 1.0 * (55000 - 50000) = 5000
        assert!((upnl - 5_000_000_000).abs() < 1_000_000);
    }

    #[test]
    fn test_unrealized_pnl_short() {
        let mut p = Position::new(
            Symbol::from("BTC-USDT"),
            Quantity::from_f64(-1.0),
            Price::from_f64(50_000.0),
        );
        p.market_price = Some(Price::from_f64(45_000.0));
        // -1.0 * (45000 - 50000) = 5000 (空头盈利)
        let upnl = p.unrealized_pnl();
        assert!(upnl > 0);
    }

    #[test]
    fn test_unrealized_pnl_no_market_price() {
        let p = Position::new(
            Symbol::from("BTC-USDT"),
            Quantity::from_f64(1.0),
            Price::from_f64(50_000.0),
        );
        assert_eq!(p.unrealized_pnl(), 0);
    }

    #[test]
    fn test_cost_basis() {
        let p = Position::new(
            Symbol::from("BTC-USDT"),
            Quantity::from_f64(2.0),
            Price::from_f64(50_000.0),
        );
        // 2.0 * 50000.0 = 100000.0
        assert!((p.cost_basis() - 100_000_000_000).abs() < 1_000_000);
    }

    #[test]
    fn test_default_via_derive() {
        // `#[derive(Default)]` 自动为所有字段使用默认值
        let p = Position::default();
        assert_eq!(p.symbol, Symbol::default());
        assert_eq!(p.quantity, Quantity::default());
        assert_eq!(p.avg_cost, Price::default());
        assert!(p.market_price.is_none());
        assert_eq!(p.realized_pnl, 0);
        assert_eq!(p.side, Side::default());
    }
}
