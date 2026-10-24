//! 市场数据事件

use serde::{Deserialize, Serialize};

use crate::market::{Bar, OrderBookSnapshot, Tick};
use crate::time::Timestamp;
use crate::types::Instrument;

/// 市场数据事件
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketDataEvent {
    /// 事件序列号
    pub seq: u64,
    /// 事件时间戳
    pub timestamp: Timestamp,
    /// 事件载荷
    pub payload: MarketDataPayload,
}

/// 市场数据事件载荷
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MarketDataPayload {
    /// 逐笔成交
    Tick(Tick),
    /// K线
    Bar(Bar),
    /// 订单簿快照
    OrderBookSnapshot(OrderBookSnapshot),
    /// 收市 K 线（统一内核的唯一决策入口）
    ///
    /// `bar.timestamp` 是 **bar 的起始时间**（不是收市时刻），收市时刻由内核用
    /// `interval` 推导（`bar.timestamp + interval`）。直接拿 `bar.timestamp` 当
    /// 事件时间戳会把收市决策排到 bar 内 tick 之前，造成前视偏差。
    ///
    /// `interval` 与 `axon-exchange` 的 `Kline.interval` 同一表示（`"1m"` / `"15m"` / `"1h"`）。
    BarClosed {
        /// 交易品种
        instrument: Instrument,
        /// K 线周期（`"1m"` / `"15m"` / `"1h"`）
        interval: String,
        /// 已收市的 OHLCV K 线
        bar: Bar,
    },
}

impl MarketDataEvent {
    /// 创建市场数据事件
    pub fn new(seq: u64, timestamp: Timestamp, payload: MarketDataPayload) -> Self {
        Self {
            seq,
            timestamp,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::Side;
    use crate::types::{Price, Quantity, SpotInstrument, Symbol};

    #[test]
    fn test_market_data_event_creation() {
        let ts = Timestamp::from_nanos(1_000);
        let tick = Tick::new(
            ts,
            Price::from_f64(100.0),
            Quantity::from_f64(10.0),
            Side::Buy,
        );
        let event = MarketDataEvent::new(0, ts, MarketDataPayload::Tick(tick));
        assert_eq!(event.seq, 0);
        assert_eq!(event.timestamp, ts);
    }

    #[test]
    fn test_bar_closed_serde_roundtrip() {
        let bar_ts = Timestamp::from_nanos(1_700_000_000_000_000_000);
        let payload = MarketDataPayload::BarClosed {
            instrument: Instrument::Spot(SpotInstrument {
                base: Symbol::from("BTC"),
                quote: Symbol::from("USDT"),
            }),
            interval: "1m".to_string(),
            bar: Bar {
                timestamp: bar_ts,
                open: Price::from_f64(100.0),
                high: Price::from_f64(101.0),
                low: Price::from_f64(99.5),
                close: Price::from_f64(100.5),
                volume: Quantity::from_f64(12.0),
            },
        };
        // 事件时间戳是收市时刻，bar.timestamp 仍是 bar 起始时间
        let close_ts = Timestamp::from_nanos(bar_ts.nanos + 60_000_000_000);
        let event = MarketDataEvent::new(0, close_ts, payload);

        let json = serde_json::to_string(&event).expect("序列化失败");
        let restored: MarketDataEvent = serde_json::from_str(&json).expect("反序列化失败");

        assert_eq!(event, restored);
    }
}
