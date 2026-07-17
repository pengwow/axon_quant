//! 事件类型分类位掩码与统一事件枚举
//!
//! [`EventType`] 是 1 字节位掩码，支持快速过滤；
//! [`Event`] 是 5 路枚举（市场数据 / 订单 / 成交 / 标记 / 系统）。

use std::fmt;
use std::ops::{BitAnd, BitOr};

use serde::{Deserialize, Serialize};

use super::fill::FillEvent;
use super::mark::MarkEvent;
use super::market::MarketDataEvent;
use super::order::OrderEvent;
use super::system::SystemEvent;
use crate::time::Timestamp;

/// 事件类型分类位掩码
///
/// 4 个分类用 4 个位表示，支持按位组合/过滤。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct EventType(u8);

impl EventType {
    /// 市场数据事件
    pub const MARKET_DATA: EventType = EventType(0b0001);
    /// 订单事件
    pub const ORDER: EventType = EventType(0b0010);
    /// 成交事件
    pub const FILL: EventType = EventType(0b0100);
    /// 系统事件
    pub const SYSTEM: EventType = EventType(0b1000);
    /// 标记价格事件
    pub const MARK: EventType = EventType(0b10000);
    /// 所有事件类型
    pub const ALL: EventType = EventType(0b11111);
    /// 空（不订阅任何事件）
    pub const NONE: EventType = EventType(0b0000);

    /// 创建自定义位掩码
    #[inline]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// 获取底层位值
    #[inline]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// 检查是否包含 `other` 的所有位
    #[inline]
    pub fn contains(self, other: EventType) -> bool {
        (self.0 & other.0) == other.0
    }

    /// 位或
    #[inline]
    pub fn union(self, other: EventType) -> EventType {
        EventType(self.0 | other.0)
    }

    /// 位与（返回非空布尔值）
    #[inline]
    pub fn intersects(self, other: EventType) -> bool {
        (self.0 & other.0) != 0
    }

    /// 是否为空
    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl Default for EventType {
    fn default() -> Self {
        Self::ALL
    }
}

impl BitOr for EventType {
    type Output = EventType;

    fn bitor(self, rhs: Self) -> Self::Output {
        EventType(self.0 | rhs.0)
    }
}

impl BitAnd for EventType {
    type Output = bool;

    fn bitand(self, rhs: Self) -> Self::Output {
        (self.0 & rhs.0) != 0
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.contains(Self::MARKET_DATA) {
            parts.push("MARKET_DATA");
        }
        if self.contains(Self::ORDER) {
            parts.push("ORDER");
        }
        if self.contains(Self::FILL) {
            parts.push("FILL");
        }
        if self.contains(Self::SYSTEM) {
            parts.push("SYSTEM");
        }
        if self.contains(Self::MARK) {
            parts.push("MARK");
        }
        if parts.is_empty() {
            write!(f, "NONE")
        } else {
            write!(f, "{}", parts.join("|"))
        }
    }
}

/// 统一事件枚举
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Event {
    /// 市场数据事件
    MarketData(MarketDataEvent),
    /// 订单事件
    Order(OrderEvent),
    /// 成交事件
    Fill(FillEvent),
    /// 标记价格事件
    Mark(MarkEvent),
    /// 系统事件
    System(SystemEvent),
}

impl Event {
    /// 获取事件时间戳
    #[inline]
    pub fn timestamp(&self) -> Timestamp {
        match self {
            Self::MarketData(e) => e.timestamp,
            Self::Order(e) => e.timestamp,
            Self::Fill(e) => e.timestamp,
            Self::Mark(e) => e.timestamp,
            Self::System(e) => e.timestamp,
        }
    }

    /// 获取事件序列号
    #[inline]
    pub fn seq(&self) -> u64 {
        match self {
            Self::MarketData(e) => e.seq,
            Self::Order(e) => e.seq,
            Self::Fill(e) => e.seq,
            // MarkEvent 不携带序列号(由外部数据源推入,无单一时序保证)
            Self::Mark(_) => 0,
            Self::System(e) => e.seq,
        }
    }

    /// 获取事件类型分类
    #[inline]
    pub fn event_type(&self) -> EventType {
        match self {
            Self::MarketData(_) => EventType::MARKET_DATA,
            Self::Order(_) => EventType::ORDER,
            Self::Fill(_) => EventType::FILL,
            Self::Mark(_) => EventType::MARK,
            Self::System(_) => EventType::SYSTEM,
        }
    }

    /// 检查事件是否在 `other` 之前
    ///
    /// 时间戳相同则用序列号决定顺序（严格全序）。
    #[inline]
    pub fn is_before(&self, other: &Event) -> bool {
        if self.timestamp() == other.timestamp() {
            self.seq() < other.seq()
        } else {
            self.timestamp() < other.timestamp()
        }
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MarketData(e) => write!(f, "MarketData(seq={}, t={})", e.seq, e.timestamp),
            Self::Order(e) => write!(
                f,
                "Order(seq={}, id={}, t={})",
                e.seq, e.order_id, e.timestamp
            ),
            Self::Fill(e) => write!(f, "Fill(seq={}, t={})", e.seq, e.timestamp),
            Self::Mark(e) => write!(
                f,
                "Mark(seq=0, instrument={:?}, price={}, t={})",
                e.instrument, e.mark_price, e.timestamp
            ),
            Self::System(e) => write!(f, "System(seq={}, t={})", e.seq, e.timestamp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_default_is_all() {
        assert_eq!(EventType::default(), EventType::ALL);
    }

    #[test]
    fn test_event_type_contains() {
        let combined = EventType::MARKET_DATA | EventType::ORDER;
        assert!(combined.contains(EventType::MARKET_DATA));
        assert!(combined.contains(EventType::ORDER));
        assert!(!combined.contains(EventType::FILL));
        assert!(combined.contains(EventType::MARKET_DATA | EventType::ORDER));
    }

    #[test]
    fn test_event_type_intersects() {
        let combined = EventType::MARKET_DATA | EventType::ORDER;
        assert!(combined & EventType::MARKET_DATA);
        assert!(combined & EventType::ORDER);
        assert!(!(combined & EventType::FILL));
        assert!(!(EventType::NONE & EventType::MARKET_DATA));
    }

    #[test]
    fn test_event_type_union() {
        let a = EventType::MARKET_DATA;
        let b = EventType::ORDER;
        let c = a.union(b);
        assert_eq!(c, EventType::MARKET_DATA | EventType::ORDER);
    }

    #[test]
    fn test_event_type_is_empty() {
        assert!(EventType::NONE.is_empty());
        assert!(!EventType::MARKET_DATA.is_empty());
        assert!(!EventType::ALL.is_empty());
    }

    #[test]
    fn test_event_type_display() {
        assert_eq!(format!("{}", EventType::MARKET_DATA), "MARKET_DATA");
        assert_eq!(
            format!("{}", EventType::MARKET_DATA | EventType::ORDER),
            "MARKET_DATA|ORDER"
        );
        assert_eq!(
            format!("{}", EventType::ALL),
            "MARKET_DATA|ORDER|FILL|SYSTEM|MARK"
        );
        assert_eq!(format!("{}", EventType::NONE), "NONE");
    }

    #[test]
    fn test_event_type_from_bits() {
        let et = EventType::from_bits(0b0101);
        assert_eq!(et, EventType::MARKET_DATA | EventType::FILL);
    }

    #[test]
    fn test_event_type_bits() {
        assert_eq!(EventType::MARKET_DATA.bits(), 0b0001);
        assert_eq!(EventType::ALL.bits(), 0b11111);
    }

    #[test]
    fn test_event_type_mark_bit_distinct() {
        // MARK 必须独立于其它 4 位,否则 EventType::MARKET_DATA | EventType::MARK
        // 会和现有分类冲突
        let combined = EventType::MARKET_DATA | EventType::MARK;
        assert!(combined.contains(EventType::MARK));
        assert!(combined.contains(EventType::MARKET_DATA));
        assert!(!combined.contains(EventType::ORDER));
        assert_eq!(EventType::MARK.bits(), 0b10000);
    }

    #[test]
    fn test_event_mark_variant() {
        use crate::types::SpotInstrument;
        use crate::types::Symbol;
        let mark = crate::event::mark::MarkEvent {
            instrument: crate::types::Instrument::Spot(SpotInstrument {
                base: Symbol::from("BTC"),
                quote: Symbol::from("USDT"),
            }),
            mark_price: crate::types::Price::from_f64(50_000.0),
            timestamp: Timestamp::from_nanos(1_000),
        };
        let evt = Event::Mark(mark);
        assert_eq!(evt.timestamp(), Timestamp::from_nanos(1_000));
        assert_eq!(evt.event_type(), EventType::MARK);
        // MarkEvent 不携带序列号
        assert_eq!(evt.seq(), 0);
    }
}
