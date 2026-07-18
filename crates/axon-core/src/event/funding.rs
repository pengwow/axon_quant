//! Funding rate 结算事件(永续合约资金费率)
//!
//! 永续合约(perp)市场通过"资金费率"机制让 perp 价格向 spot 收敛:
//! - 资金费率 > 0:long 付 short(perp 高于 spot,空方更乐观,空方收钱)
//! - 资金费率 < 0:short 付 long(perp 低于 spot,多方更乐观,多方收钱)
//!
//! 主要交易所(Binance / OKX / Bybit)典型每 8 小时结算一次
//! (00:00 / 08:00 / 16:00 UTC),本框架**不**强制 8h 调度,只提供
//! 事件协议,用户从数据源/调度器按需 push。
//!
//! # 结算数学
//!
//! 公式:`cash_delta = position_qty * funding_rate * mark_price`
//! - position_qty 带符号:long 为正,short 为负
//! - funding_rate 带符号:正费率 = long 付 / short 收
//! - 例如:long 0.5 @ funding 0.0001(0.01%) @ mark 50000
//!   → cash_delta = 0.5 × 0.0001 × 50000 = -2.5(付出 2.5 USDT)
//!
//! 引擎派发逻辑:见 `axon_backtest::engine::BacktestEngine::handle_funding`。
//!
//! 0.5.0 新增(Phase C):FundingEvent 类型 + 派发 + 现金扣减。

use serde::{Deserialize, Serialize};

use crate::time::Timestamp;
use crate::types::{Instrument, Price};

/// Funding rate 结算事件(永续合约资金费率)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FundingEvent {
    /// 永续合约品种(只对 swap 生效,spot 收到会忽略)
    pub instrument: Instrument,
    /// 资金费率(正数 = long 付 short,负数 = short 付 long)
    ///
    /// 典型范围 -0.003 ~ +0.003(±0.3%);常见 ±0.0001(±0.01%)
    ///
    /// 注:`f64` 不可 derive `Eq`(NaN ≠ NaN),本结构只用 `PartialEq` 比较即可;
    /// `Hash` 也未 derive 因为 `HashMap<Instrument, _>` 的 key 仅用 `Instrument`。
    pub funding_rate: f64,
    /// 结算时 mark 价(用于结算金额计算)
    pub mark_price: Price,
    /// 结算时间戳
    pub timestamp: Timestamp,
}

impl FundingEvent {
    /// 创建 Funding 事件
    pub fn new(
        instrument: Instrument,
        funding_rate: f64,
        mark_price: Price,
        timestamp: Timestamp,
    ) -> Self {
        Self {
            instrument,
            funding_rate,
            mark_price,
            timestamp,
        }
    }

    /// 计算某持仓的 funding 现金变动
    ///
    /// 公式:`cash_delta = -position_qty * funding_rate * mark_price`
    ///
    /// 符号语义(正 funding 表示"long 付 / short 收",业内标准):
    /// - long (+qty) + 正 funding → cash_delta < 0(long 付,公式取负号)
    /// - long (+qty) + 负 funding → cash_delta > 0(long 收)
    /// - short (-qty) + 正 funding → cash_delta > 0(short 收,负 × 负 = 正)
    /// - short (-qty) + 负 funding → cash_delta < 0(short 付)
    ///
    /// 推导:持仓每 `mark_price` 价值 `position_qty * mark_price`,funding 是按这个
    /// 名义值的 `funding_rate` 比例结算;正 funding 时 long 需付 → 乘 -1。
    ///
    /// 数值校验(看上方 `test_funding_*` 案例):
    /// - long 0.5 × 0.0001 × 50000 × (-1) = -2.5 ✓ long 付 2.5
    /// - short -0.5 × 0.0001 × 50000 × (-1) = +2.5 ✓ short 收 2.5
    pub fn cash_delta_for(&self, position_qty: f64) -> f64 {
        -position_qty * self.funding_rate * self.mark_price.as_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SpotInstrument, SwapInstrument, SwapSettle, Symbol};

    fn btc_perp() -> Instrument {
        Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        })
    }
    fn btc_spot() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    #[test]
    fn test_funding_event_creation() {
        let evt = FundingEvent::new(
            btc_perp(),
            0.0001,
            Price::from_f64(50_000.0),
            Timestamp::from_nanos(1_700_000_000_000_000_000),
        );
        assert_eq!(evt.funding_rate, 0.0001);
    }

    #[test]
    fn test_funding_cash_delta_long_pays() {
        // long 0.5 + 正 funding 0.0001 → long 付
        let evt = FundingEvent::new(
            btc_perp(),
            0.0001,
            Price::from_f64(50_000.0),
            Timestamp::from_nanos(0),
        );
        // -0.5 × 0.0001 × 50000 = -2.5(long 付 2.5,cash 减少)
        assert!((evt.cash_delta_for(0.5) - (-2.5)).abs() < 1e-9);
    }

    #[test]
    fn test_funding_cash_delta_short_receives() {
        // short -0.5 + 正 funding 0.0001 → short 收
        let evt = FundingEvent::new(
            btc_perp(),
            0.0001,
            Price::from_f64(50_000.0),
            Timestamp::from_nanos(0),
        );
        // -(-0.5) × 0.0001 × 50000 = +2.5(short 收 2.5,cash 增加)
        assert!((evt.cash_delta_for(-0.5) - 2.5).abs() < 1e-9);
    }

    #[test]
    fn test_funding_cash_delta_negative_rate() {
        // funding < 0:long 收 / short 付(perp 折价,多头受激励)
        let evt = FundingEvent::new(
            btc_perp(),
            -0.0002,
            Price::from_f64(60_000.0),
            Timestamp::from_nanos(0),
        );
        // -0.5 × -0.0002 × 60000 = +6(long 收 6)
        assert!((evt.cash_delta_for(0.5) - 6.0).abs() < 1e-9);
        // -(-0.5) × -0.0002 × 60000 = -6(short 付 6)
        assert!((evt.cash_delta_for(-0.5) - (-6.0)).abs() < 1e-9);
    }

    #[test]
    fn test_funding_event_serde() {
        let evt = FundingEvent::new(
            btc_perp(),
            0.0001,
            Price::from_f64(50_000.0),
            Timestamp::from_nanos(0),
        );
        let json = serde_json::to_string(&evt).unwrap();
        let parsed: FundingEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(evt, parsed);
    }

    #[test]
    #[allow(clippy::redundant_clone)]
    fn test_funding_spot_instrument_accepted() {
        // 引擎派发时应忽略 spot(只对 swap 结算)
        // 这里只验证 FundingEvent 可携带 spot instrument(类型层允许)
        let _evt = FundingEvent::new(
            btc_spot(),
            0.0001,
            Price::from_f64(50_000.0),
            Timestamp::from_nanos(0),
        );
    }
}
