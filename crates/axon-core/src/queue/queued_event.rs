//! 队列条目：封装事件 + 排序元数据

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::event::Event;
use crate::time::Timestamp;

/// 事件队列条目：封装事件 + 排序元数据
///
/// BinaryHeap 是最大堆；通过反转 `Ord` 实现最小堆语义。
/// 排序规则：`timestamp` 升序 → `seq` 升序。
///
/// 注:0.5.0 起去掉了 `Eq` derive(因 `Event::Funding` 含 `f64`),只用 `PartialEq`
/// 比较;`Ord` 是手写的(基于 timestamp + seq,不依赖 `Event::eq`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedEvent {
    /// 事件发生时间
    pub timestamp: Timestamp,
    /// 序列号：同一时间戳内按此排序
    pub seq: u64,
    /// 事件载荷
    pub event: Event,
}

impl PartialOrd for QueuedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueuedEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        // 反转比较以实现最小堆（BinaryHeap 是最大堆）
        other
            .timestamp
            .cmp(&self.timestamp)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

// 0.5.0:由于 `Event` 不再是 `Eq`(含 `f64`),`QueuedEvent` 也不能 derive `Eq`。
// 手写 `Eq` 时,基于已 derived 的 `PartialEq`(`f64` 字段在相等比较上 NaN ≠ NaN,
// 但对有序队列而言,NaN 仍可参与排序 — Ord 是手写的,不依赖 `Event::eq`)。
impl Eq for QueuedEvent {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::SystemEvent;
    use crate::event::system::SystemAction;
    use std::collections::BinaryHeap;

    fn make_event(seq: u64, ts_nanos: i64) -> QueuedEvent {
        QueuedEvent {
            timestamp: Timestamp::from_nanos(ts_nanos),
            seq,
            event: Event::System(SystemEvent::new(
                seq,
                Timestamp::from_nanos(ts_nanos),
                SystemAction::Heartbeat,
            )),
        }
    }

    #[test]
    fn test_ord_earlier_timestamp_is_less() {
        let mut heap = BinaryHeap::new();
        heap.push(make_event(0, 1_000));
        heap.push(make_event(0, 500));
        // 弹出最小（最早时间）
        let first = heap.pop().unwrap();
        assert_eq!(first.timestamp, Timestamp::from_nanos(500));
    }

    #[test]
    fn test_ord_same_timestamp_smaller_seq_first() {
        let mut heap = BinaryHeap::new();
        heap.push(make_event(2, 1_000));
        heap.push(make_event(1, 1_000));
        // 弹出最小（同一时间戳内 seq 较小者）
        let first = heap.pop().unwrap();
        assert_eq!(first.seq, 1);
    }

    #[test]
    fn test_partial_eq() {
        let a = make_event(1, 1_000);
        let b = make_event(1, 1_000);
        assert_eq!(a, b);
    }
}
