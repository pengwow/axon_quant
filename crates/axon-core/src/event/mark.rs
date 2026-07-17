//! Mark price 事件(标记价格更新)
//!
//! 由外部数据源推入,引擎在 `dispatch` 时写入 `mark_cache`,
//! 供未来 funding 结算 / unrealized PnL 计算使用。
//!
//! 本次 spec 范围:仅写缓存,不触 NAV 重采样。
//! 详见 spec §4.4。

use serde::{Deserialize, Serialize};

use crate::time::Timestamp;
use crate::types::{Instrument, Price};

/// Mark price 事件(标记价格更新)
///
/// 由外部数据源推入,引擎在 `dispatch` 时写入 `mark_cache`,
/// 供未来 funding 结算 / unrealized PnL 计算使用。
///
/// 本次 spec 范围:仅写缓存,不触 NAV 重采样。
/// 详见 spec §4.4。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkEvent {
    /// 品种
    pub instrument: Instrument,
    /// 标记价格
    pub mark_price: Price,
    /// 时间戳
    pub timestamp: Timestamp,
}

impl MarkEvent {
    /// 创建 Mark 事件
    pub fn new(instrument: Instrument, mark_price: Price, timestamp: Timestamp) -> Self {
        Self {
            instrument,
            mark_price,
            timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SpotInstrument, SwapInstrument, SwapSettle, Symbol};

    #[test]
    fn test_mark_event_creation() {
        let evt = MarkEvent::new(
            Instrument::Spot(SpotInstrument {
                base: Symbol::from("BTC"),
                quote: Symbol::from("USDT"),
            }),
            Price::from_f64(50_000.0),
            Timestamp::from_nanos(1_700_000_000_000_000_000),
        );
        assert_eq!(evt.mark_price.as_f64(), 50_000.0);
    }

    #[test]
    fn test_mark_event_serde() {
        let evt = MarkEvent::new(
            Instrument::Swap(SwapInstrument {
                base: Symbol::from("ETH"),
                quote: Symbol::from("USDT"),
                settle: SwapSettle::UsdMargin,
                contract_size: 1.0,
            }),
            Price::from_f64(3_000.0),
            Timestamp::from_nanos(0),
        );
        let json = serde_json::to_string(&evt).unwrap();
        let parsed: MarkEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(evt, parsed);
    }
}
