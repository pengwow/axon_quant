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

/// 0.6.0 新增(Phase 2):funding 8h 自动调度配置
///
/// 引擎在每根 bar 末(`begin_bar` 收尾)检查 schedule:若
/// `last_funding_ts[instrument] + interval_ns <= bar_ts` 合成 `FundingEvent`
/// 推入 `EventQueue`(走 `push_funding` 派发路径)。
///
/// `mark_aware = true` 时使用 `mark_cache[instrument]`(若 cache 为空则
/// fallback 0.0,真实回测中 mark 缺失等价 funding 不发生);`false` 时用
/// 当前 `fallback_mark` 价(通常为 fill_price,需用户先 fill 过)。
///
/// 真实交易所(Binance / OKX)典型 8h 一次,UTC 00:00 / 08:00 / 16:00 整点;
/// `interval_ns` 是**相对**间隔,不强制对齐到整点(用户可用 `next_funding_ts`
/// 自定义对齐逻辑)。
///
/// # 示例
///
/// ```ignore
/// use axon_core::event::FundingSchedule;
/// use axon_core::types::{Instrument, SwapInstrument, SwapSettle, Symbol};
///
/// let btc_perp = Instrument::Swap(SwapInstrument {
///     base: Symbol::from("BTC"),
///     quote: Symbol::from("USDT"),
///     settle: SwapSettle::UsdMargin,
///     contract_size: 1.0,
/// });
/// let schedule = FundingSchedule::fixed_8h(btc_perp, 0.0001);
/// assert_eq!(schedule.interval_ns, 8 * 3600 * 1_000_000_000);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FundingSchedule {
    /// 永续合约品种(只对 swap 生效;spot 收到会被引擎忽略)
    pub instrument: Instrument,
    /// 结算间隔(ns)。典型 8h = 28_800_000_000_000
    pub interval_ns: i64,
    /// 资金费率(正 = long 付 short,负 = short 付 long)
    ///
    /// 回测常用固定费率;若需历史回放真实 funding,改用 `Event::Funding`
    /// 直接推入(`with_funding_schedule` 关闭后用 `push_funding`)。
    pub fixed_rate: f64,
    /// `true`:用 `mark_cache[instrument]`(推荐);`false`:fallback 0
    pub mark_aware: bool,
}

impl FundingSchedule {
    /// 构造 8h 固定费率 schedule(回测最常用)
    ///
    /// 默认 `mark_aware = true`,依赖引擎 mark cache。
    pub fn fixed_8h(instrument: Instrument, rate: f64) -> Self {
        Self {
            instrument,
            interval_ns: 8 * 3600 * 1_000_000_000,
            fixed_rate: rate,
            mark_aware: true,
        }
    }

    /// 计算下一次 funding 时间戳(相对 `prev_ts + interval_ns`)
    ///
    /// 不强制对齐到整点;若用户要 UTC 00:00 / 08:00 / 16:00 对齐,自行
    /// 在 `last_funding_ts` 初始化时 set 0(默认 0,首次 bar 末即触发)。
    #[inline]
    pub fn next_funding_ts(&self, prev_ts: Timestamp) -> Timestamp {
        Timestamp::from_nanos(prev_ts.nanos + self.interval_ns)
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::*;
    use crate::types::{SwapInstrument, SwapSettle, Symbol};

    fn btc_perp() -> Instrument {
        Instrument::Swap(SwapInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
            settle: SwapSettle::UsdMargin,
            contract_size: 1.0,
        })
    }

    #[test]
    fn test_fixed_8h_default_interval() {
        let s = FundingSchedule::fixed_8h(btc_perp(), 0.0001);
        assert_eq!(s.interval_ns, 28_800_000_000_000);
        assert!((s.fixed_rate - 0.0001).abs() < 1e-12);
        assert!(s.mark_aware);
    }

    #[test]
    fn test_next_funding_ts_adds_interval() {
        let s = FundingSchedule::fixed_8h(btc_perp(), 0.0);
        let next = s.next_funding_ts(Timestamp::from_nanos(0));
        assert_eq!(next.nanos, 28_800_000_000_000);
    }
}
