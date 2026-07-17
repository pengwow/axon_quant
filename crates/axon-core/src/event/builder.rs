//! 事件构建器（类型安全的事件创建 + 自增序列号）

use super::fill::FillEvent;
use super::funding::FundingEvent;
use super::mark::MarkEvent;
use super::market::{MarketDataEvent, MarketDataPayload};
use super::order::{OrderAction, OrderEvent};
use super::system::{SystemAction, SystemEvent};
use super::types::Event;
use crate::market::Trade;
use crate::order::OrderId;
use crate::time::Timestamp;

/// 事件构建器
///
/// 持有自增序列号，提供 4 种事件类型的便捷构造方法。
pub struct EventBuilder {
    /// 下一个待分配的序列号
    next_seq: u64,
}

impl EventBuilder {
    /// 创建事件构建器
    pub fn new(start_seq: u64) -> Self {
        Self {
            next_seq: start_seq,
        }
    }

    /// 构建市场数据事件（序列号自增）
    pub fn market_data(&mut self, timestamp: Timestamp, payload: MarketDataPayload) -> Event {
        let seq = self.next_seq;
        self.next_seq += 1;
        Event::MarketData(MarketDataEvent::new(seq, timestamp, payload))
    }

    /// 构建订单事件
    pub fn order(&mut self, timestamp: Timestamp, order_id: OrderId, action: OrderAction) -> Event {
        let seq = self.next_seq;
        self.next_seq += 1;
        Event::Order(OrderEvent {
            seq,
            timestamp,
            order_id,
            action,
        })
    }

    /// 构建成交事件
    pub fn fill(&mut self, timestamp: Timestamp, trade: Trade) -> Event {
        let seq = self.next_seq;
        self.next_seq += 1;
        Event::Fill(FillEvent::new(seq, timestamp, trade))
    }

    /// 构建 Mark 事件(标记价格更新)— T3.6 新增
    ///
    /// 用法:`b.mark(MarkEvent::new(inst, price, ts))`,`MarkEvent` 自身已含
    /// timestamp,这里 `next_seq` 仅自增以保持 builder 全局序号一致(Event::Mark
    /// 序列化时 `seq()` 返回 0,但 builder 仍要分配以避免后续 `current_seq` 跳变)。
    pub fn mark(&mut self, mark: MarkEvent) -> Event {
        let seq = self.next_seq;
        self.next_seq += 1;
        let _ = seq;
        Event::Mark(mark)
    }

    /// 构建 Funding 事件(永续合约资金费率结算)— 0.5.0 新增(Phase C)
    ///
    /// 用法:`b.funding(FundingEvent::new(inst, rate, mark, ts))`,`FundingEvent`
    /// 自身已含 timestamp,这里 `next_seq` 仅自增(同 `mark`,因为外部数据源推入的
    /// FundingEvent 不携带全局递增 seq;`Event::Funding.seq()` 仍返回 0)。
    pub fn funding(&mut self, funding: FundingEvent) -> Event {
        let seq = self.next_seq;
        self.next_seq += 1;
        let _ = seq;
        Event::Funding(funding)
    }

    /// 构建系统事件
    pub fn system(&mut self, timestamp: Timestamp, action: SystemAction) -> Event {
        let seq = self.next_seq;
        self.next_seq += 1;
        Event::System(SystemEvent::new(seq, timestamp, action))
    }

    /// 获取下一个待分配的序列号
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// 获取当前已分配的最大序列号（`next_seq - 1`）
    pub fn current_seq(&self) -> u64 {
        self.next_seq.saturating_sub(1)
    }
}

impl Default for EventBuilder {
    fn default() -> Self {
        Self::new(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market::{Side, Tick};
    use crate::types::{Price, Quantity};

    #[test]
    fn test_builder_seq_monotonic() {
        let mut b = EventBuilder::new(10);
        let ts = Timestamp::from_nanos(0);
        let e1 = b.market_data(
            ts,
            MarketDataPayload::Tick(Tick::new(
                ts,
                Price::from_f64(100.0),
                Quantity::from_f64(1.0),
                Side::Buy,
            )),
        );
        assert_eq!(e1.seq(), 10);
        let e2 = b.system(ts, SystemAction::Heartbeat);
        assert_eq!(e2.seq(), 11);
        let e3 = b.system(ts, SystemAction::Heartbeat);
        assert_eq!(e3.seq(), 12);
        assert_eq!(b.next_seq(), 13);
        assert_eq!(b.current_seq(), 12);
    }

    #[test]
    fn test_builder_default() {
        let b = EventBuilder::default();
        assert_eq!(b.next_seq(), 0);
    }

    #[test]
    fn test_builder_event_type() {
        let mut b = EventBuilder::new(0);
        let ts = Timestamp::from_nanos(0);
        assert_eq!(
            b.market_data(
                ts,
                MarketDataPayload::Tick(Tick::new(
                    ts,
                    Price::from_f64(100.0),
                    Quantity::from_f64(1.0),
                    Side::Buy,
                ))
            )
            .event_type(),
            super::super::types::EventType::MARKET_DATA
        );
        assert_eq!(
            b.system(ts, SystemAction::Heartbeat).event_type(),
            super::super::types::EventType::SYSTEM
        );
    }

    // ─── 边界测试 ──────────────────────────────────────

    /// 从接近 `u64::MAX` 启动：达到 `u64::MAX` 边界附近能正常分配
    /// （连续分配超过 `u64::MAX` 个事件会触发 overflow；这是当前实现的设计，
    /// 生产环境应避免在长生命周期进程中无限制构造事件）
    #[test]
    fn test_builder_start_at_max_seq_saturates() {
        // 从 u64::MAX - 1 启动：单次分配能正常使用 u64::MAX - 1
        let mut b = EventBuilder::new(u64::MAX - 1);
        let ts = Timestamp::from_nanos(0);
        let evt = b.system(ts, SystemAction::Heartbeat);
        assert_eq!(evt.seq(), u64::MAX - 1);
        // current_seq 仍可正确读取最近一次分配的 seq
        assert_eq!(b.current_seq(), u64::MAX - 1);
    }

    /// 从 `u64::MAX` 启动会触发溢出 panic（已知设计约束）
    ///
    /// 当前实现 `next_seq += 1` 在 debug 模式下会因加法溢出而 panic。
    /// 这是有意的"硬停止"行为，迫使上游在接近 seq 极限时主动重置
    /// （生产环境应监控 next_seq 接近 u64::MAX 时切换到新的 EventBuilder）。
    #[test]
    #[should_panic(expected = "attempt to add with overflow")]
    fn test_builder_start_at_max_seq_panics_on_overflow() {
        let mut b = EventBuilder::new(u64::MAX);
        let ts = Timestamp::from_nanos(0);
        let _ = b.system(ts, SystemAction::Heartbeat);
    }

    /// current_seq 在未分配时回退为 0（saturating_sub 防下溢）
    #[test]
    fn test_builder_current_seq_saturates_at_zero() {
        let b = EventBuilder::new(0);
        // 尚未分配任何 seq ⇒ current_seq = 0（不会下溢到 u64::MAX）
        assert_eq!(b.current_seq(), 0);
    }

    /// 大量连续构造（10 万）⇒ seq 严格单调
    #[test]
    fn test_builder_high_volume_monotonic() {
        let mut b = EventBuilder::new(0);
        let ts = Timestamp::from_nanos(0);
        // 首次分配 seq = 0 ⇒ 先记录 prev，从第二次开始检查严格递增
        let evt0 = b.system(ts, SystemAction::Heartbeat);
        let mut prev = evt0.seq();
        for _ in 1..100_000 {
            let evt = b.system(ts, SystemAction::Heartbeat);
            assert!(evt.seq() > prev, "seq 必须严格递增");
            prev = evt.seq();
        }
        assert_eq!(b.next_seq(), 100_000);
    }

    /// 自定义起始 seq：跳过前 N 个位置
    #[test]
    fn test_builder_start_at_arbitrary_seq() {
        let mut b = EventBuilder::new(1_000_000);
        let ts = Timestamp::from_nanos(0);
        let evt = b.system(ts, SystemAction::Heartbeat);
        assert_eq!(evt.seq(), 1_000_000);
        assert_eq!(b.next_seq(), 1_000_001);
    }
}
