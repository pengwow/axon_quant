//! 事件队列主结构

use std::collections::BinaryHeap;

use serde::{Deserialize, Serialize};

use super::error::EventQueueResult;
use super::mode::QueueMode;
use super::queued_event::QueuedEvent;
use super::stats::QueueStats;
use crate::event::Event;
use crate::time::Timestamp;

/// 事件队列：回测引擎的时间调度核心
///
/// 基于 `BinaryHeap` 的最小堆实现（通过反转 `Ord`）。
/// 同一时间戳的事件按 `seq` 升序出队（FIFO 语义）。
#[derive(Debug, Serialize, Deserialize)]
pub struct EventQueue {
    /// BinaryHeap 优先级队列
    events: BinaryHeap<QueuedEvent>,
    /// 当前模拟时间（最后一次出队的时间）
    current_time: Timestamp,
    /// 全局序列号计数器
    seq_counter: u64,
    /// 运行模式
    mode: QueueMode,
    /// 重放日志：记录所有入队事件，用于 `reset` 后重放
    /// 只存储 Event（不含 timestamp/seq），replay 时从索引重建
    replay_log: Vec<Event>,
    /// 是否启用重放日志
    enable_replay_log: bool,
    /// 统计信息
    stats: QueueStats,
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl EventQueue {
    /// 创建空事件队列
    pub fn new() -> Self {
        Self {
            events: BinaryHeap::new(),
            current_time: Timestamp::from_nanos(0),
            seq_counter: 0,
            mode: QueueMode::Normal,
            replay_log: Vec::new(),
            enable_replay_log: false,
            stats: QueueStats::default(),
        }
    }

    /// 创建带重放日志的事件队列
    pub fn with_replay_log() -> Self {
        Self {
            enable_replay_log: true,
            ..Self::new()
        }
    }

    /// 分配下一个序列号
    #[inline]
    fn next_seq(&mut self) -> u64 {
        let seq = self.seq_counter;
        self.seq_counter += 1;
        seq
    }

    /// 入队单个事件（时间戳从事件本身读取）
    pub fn push(&mut self, event: Event) {
        let ts = event.timestamp();
        // replay_log 只存 Event，避免克隆 timestamp+seq
        if self.enable_replay_log {
            self.replay_log.push(event.clone());
        }
        let queued = QueuedEvent {
            timestamp: ts,
            seq: self.next_seq(),
            event,
        };
        self.events.push(queued);
        self.stats.total_pushed += 1;
    }

    /// 入队单个事件（指定时间戳，用于外部数据注入）
    pub fn push_at(&mut self, timestamp: Timestamp, event: Event) {
        // replay_log 只存 Event，避免克隆 timestamp+seq
        if self.enable_replay_log {
            self.replay_log.push(event.clone());
        }
        let queued = QueuedEvent {
            timestamp,
            seq: self.next_seq(),
            event,
        };
        self.events.push(queued);
        self.stats.total_pushed += 1;
    }

    /// 批量入队
    pub fn push_batch(&mut self, events: Vec<Event>) {
        self.events.reserve(events.len());
        for event in events {
            self.push(event);
        }
    }

    /// 从排序好的事件列表批量加载
    pub fn from_sorted(events: Vec<Event>) -> Self {
        let len = events.len();
        let mut queue = Self::new();
        queue.events = BinaryHeap::with_capacity(len);
        queue.stats.total_pushed = len as u64;

        for event in events {
            let ts = event.timestamp();
            let queued = QueuedEvent {
                timestamp: ts,
                seq: queue.next_seq(),
                event,
            };
            queue.events.push(queued);
        }

        queue
    }

    /// 出队：返回时间最早的事件
    #[allow(clippy::should_implement_trait)] // TDD 规范明确要求 `next` 命名
    pub fn next(&mut self) -> Option<Event> {
        if self.mode == QueueMode::Paused {
            return None;
        }

        let queued = self.events.pop()?;
        self.current_time = queued.timestamp;
        self.stats.total_popped += 1;

        if self.mode == QueueMode::StepOnce {
            self.mode = QueueMode::Paused;
        }

        Some(queued.event)
    }

    /// 查看下一个事件的时间戳（不出队）
    #[inline]
    pub fn peek_time(&self) -> Option<Timestamp> {
        self.events.peek().map(|e| e.timestamp)
    }

    /// 查看下一个事件（不出队）
    #[inline]
    pub fn peek(&self) -> Option<&QueuedEvent> {
        self.events.peek()
    }

    /// 队列是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// 队列中剩余事件数
    #[inline]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// 当前模拟时间
    #[inline]
    pub fn current_time(&self) -> Timestamp {
        self.current_time
    }

    /// 快进到指定时间：丢弃所有时间 <= target 的事件
    pub fn fast_forward_to(&mut self, target: Timestamp) -> usize {
        let mut skipped = 0;
        while let Some(peeked) = self.events.peek() {
            if peeked.timestamp > target {
                break;
            }
            self.events.pop();
            skipped += 1;
        }
        self.current_time = target;
        self.stats.total_skipped += skipped as u64;
        skipped
    }

    /// 快进并收集被跳过的事件
    pub fn fast_forward_collect(&mut self, target: Timestamp) -> Vec<Event> {
        let mut skipped = Vec::new();
        while let Some(peeked) = self.events.peek() {
            if peeked.timestamp > target {
                break;
            }
            if let Some(queued) = self.events.pop() {
                skipped.push(queued.event);
            }
        }
        self.current_time = target;
        skipped
    }

    /// 从队列中 drain 出所有时间 <= target 的事件（不更新 current_time）
    pub fn drain_until(&mut self, target: Timestamp) -> Vec<Event> {
        let mut result = Vec::new();
        while let Some(peeked) = self.events.peek() {
            if peeked.timestamp > target {
                break;
            }
            if let Some(queued) = self.events.pop() {
                result.push(queued.event);
            }
        }
        result
    }

    /// 暂停队列
    pub fn pause(&mut self) {
        self.mode = QueueMode::Paused;
    }

    /// 恢复队列
    pub fn resume(&mut self) {
        self.mode = QueueMode::Normal;
    }

    /// 单步执行：出队一个事件后自动暂停
    pub fn step(&mut self) -> Option<Event> {
        self.mode = QueueMode::StepOnce;
        self.next()
    }

    /// 获取当前模式
    pub fn mode(&self) -> QueueMode {
        self.mode
    }

    /// 重置队列：清空所有事件，恢复初始状态
    pub fn reset(&mut self) {
        self.events.clear();
        self.current_time = Timestamp::from_nanos(0);
        self.seq_counter = 0;
        self.mode = QueueMode::Normal;
        self.stats = QueueStats::default();
    }

    /// 从重放日志重建队列
    pub fn replay(&mut self) -> EventQueueResult<()> {
        if !self.enable_replay_log {
            return Err(super::error::EventQueueError::ReplayNotEnabled);
        }
        if self.replay_log.is_empty() {
            return Err(super::error::EventQueueError::ReplayLogEmpty);
        }

        self.events.clear();
        self.current_time = Timestamp::from_nanos(0);
        self.seq_counter = self.replay_log.len() as u64;
        self.mode = QueueMode::Normal;
        self.stats.replay_count += 1;

        // 从 replay_log 重建 QueuedEvent，seq 用索引保持原始顺序
        let mut indexed: Vec<(usize, Timestamp)> = self
            .replay_log
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.timestamp()))
            .collect();
        indexed.sort_by_key(|(_, ts)| *ts);

        // 反向插入构建最小堆
        for (idx, ts) in indexed.into_iter().rev() {
            let queued = QueuedEvent {
                timestamp: ts,
                seq: idx as u64,
                // clone 是不可避免的（需要同时在 heap 和 log 中）
                event: self.replay_log[idx].clone(),
            };
            self.events.push(queued);
        }

        Ok(())
    }

    /// 获取重放日志
    pub fn replay_log(&self) -> &[Event] {
        &self.replay_log
    }

    /// 清空重放日志
    pub fn clear_replay_log(&mut self) {
        self.replay_log.clear();
    }

    /// 获取统计信息
    pub fn stats(&self) -> &QueueStats {
        &self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventBuilder;
    use crate::event::system::SystemAction;
    use crate::time::Timestamp;

    fn make_event(b: &mut EventBuilder, ts_nanos: i64) -> Event {
        b.system(Timestamp::from_nanos(ts_nanos), SystemAction::Heartbeat)
    }

    #[test]
    fn test_new_queue_is_empty() {
        let mut q = EventQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert_eq!(q.current_time(), Timestamp::from_nanos(0));
        assert!(q.peek_time().is_none());
        assert!(q.next().is_none());
    }

    #[test]
    fn test_push_and_next_returns_ordered() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 乱序入队
        q.push(make_event(&mut b, 1_000));
        q.push(make_event(&mut b, 500));
        q.push(make_event(&mut b, 2_000));
        // 出队顺序：500 → 1000 → 2000
        assert_eq!(q.next().unwrap().timestamp(), Timestamp::from_nanos(500));
        assert_eq!(q.next().unwrap().timestamp(), Timestamp::from_nanos(1_000));
        assert_eq!(q.next().unwrap().timestamp(), Timestamp::from_nanos(2_000));
        assert!(q.next().is_none());
    }

    #[test]
    fn test_next_returns_none_when_empty() {
        let mut q = EventQueue::new();
        assert!(q.next().is_none());
    }

    #[test]
    fn test_peek_time_returns_next_timestamp() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(make_event(&mut b, 1_000));
        q.push(make_event(&mut b, 500));
        assert_eq!(q.peek_time(), Some(Timestamp::from_nanos(500)));
        // peek 不应改变队列
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn test_push_at_overrides_event_timestamp() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 事件本身时间戳是 100，但 push_at 用 50
        let e = b.system(Timestamp::from_nanos(100), SystemAction::Heartbeat);
        q.push_at(Timestamp::from_nanos(50), e);
        // 出队时返回的事件 timestamp 仍为 100（事件本身），但队列时间戳为 50
        let popped = q.next().unwrap();
        assert_eq!(popped.timestamp(), Timestamp::from_nanos(100));
        assert_eq!(q.current_time(), Timestamp::from_nanos(50));
    }

    #[test]
    fn test_batch_load_maintains_order() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let events = vec![
            make_event(&mut b, 3_000),
            make_event(&mut b, 1_000),
            make_event(&mut b, 2_000),
        ];
        q.push_batch(events);
        assert_eq!(q.len(), 3);
        assert_eq!(q.next().unwrap().timestamp(), Timestamp::from_nanos(1_000));
        assert_eq!(q.next().unwrap().timestamp(), Timestamp::from_nanos(2_000));
        assert_eq!(q.next().unwrap().timestamp(), Timestamp::from_nanos(3_000));
    }

    #[test]
    fn test_batch_load_large_dataset() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let mut events = Vec::with_capacity(1_000);
        // 倒序构造，模拟乱序
        for i in (0..1_000).rev() {
            events.push(make_event(&mut b, i as i64 * 1_000));
        }
        q.push_batch(events);
        assert_eq!(q.len(), 1_000);
        // 验证出队顺序严格递增
        let mut prev = -1i64;
        while let Some(e) = q.next() {
            let t = e.timestamp().nanos;
            assert!(t > prev);
            prev = t;
        }
    }

    #[test]
    fn test_from_sorted_loads_in_linear_time() {
        let mut b = EventBuilder::new(0);
        let mut events = Vec::new();
        for i in 0..100 {
            events.push(make_event(&mut b, i * 1_000));
        }
        let q = EventQueue::from_sorted(events);
        assert_eq!(q.len(), 100);
        assert_eq!(q.stats().total_pushed, 100);
    }

    #[test]
    fn test_fast_forward_skips_events() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        for i in 0..5 {
            q.push(make_event(&mut b, i * 1_000));
        }
        // 快进到 2000：应跳过 timestamp <= 2000 的事件（0, 1000, 2000 = 3 个）
        let skipped = q.fast_forward_to(Timestamp::from_nanos(2_000));
        assert_eq!(skipped, 3);
        assert_eq!(q.len(), 2);
        assert_eq!(q.current_time(), Timestamp::from_nanos(2_000));
        assert_eq!(q.stats().total_skipped, 3);
        // 下一次 next 应该是 3000
        assert_eq!(q.next().unwrap().timestamp(), Timestamp::from_nanos(3_000));
    }

    #[test]
    fn test_fast_forward_to_exact_time_includes_equal() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(make_event(&mut b, 1_000));
        q.push(make_event(&mut b, 2_000));
        // 快进到 1000，应跳过 1000（<= target 全部跳过）
        let skipped = q.fast_forward_to(Timestamp::from_nanos(1_000));
        assert_eq!(skipped, 1);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn test_fast_forward_to_empty() {
        let mut q = EventQueue::new();
        let skipped = q.fast_forward_to(Timestamp::from_nanos(1_000));
        assert_eq!(skipped, 0);
    }

    #[test]
    fn test_pause_and_resume() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(make_event(&mut b, 1_000));
        q.pause();
        assert_eq!(q.mode(), QueueMode::Paused);
        assert!(q.next().is_none());
        q.resume();
        assert_eq!(q.mode(), QueueMode::Normal);
        assert!(q.next().is_some());
    }

    #[test]
    fn test_step_executes_single_event() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(make_event(&mut b, 1_000));
        q.push(make_event(&mut b, 2_000));
        // 单步：出一个事件后暂停
        let e = q.step();
        assert!(e.is_some());
        assert_eq!(q.mode(), QueueMode::Paused);
        // 暂停后再 next 返回 None
        assert!(q.next().is_none());
    }

    #[test]
    fn test_reset_clears_queue() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(make_event(&mut b, 1_000));
        q.push(make_event(&mut b, 2_000));
        q.next(); // 推进 current_time
        q.reset();
        assert!(q.is_empty());
        assert_eq!(q.current_time(), Timestamp::from_nanos(0));
        assert_eq!(q.stats().total_pushed, 0);
    }

    #[test]
    fn test_replay_produces_same_sequence() {
        let mut q = EventQueue::with_replay_log();
        let mut b = EventBuilder::new(0);
        q.push(make_event(&mut b, 1_000));
        q.push(make_event(&mut b, 500));
        q.push(make_event(&mut b, 2_000));
        let original: Vec<i64> =
            std::iter::from_fn(|| q.next().map(|e| e.timestamp().nanos)).collect();
        assert_eq!(original, vec![500, 1_000, 2_000]);

        // 重放
        q.replay().unwrap();
        assert_eq!(q.stats().replay_count, 1);
        let replayed: Vec<i64> =
            std::iter::from_fn(|| q.next().map(|e| e.timestamp().nanos)).collect();
        assert_eq!(replayed, original);
    }

    #[test]
    fn test_replay_without_log_enabled_returns_error() {
        let mut q = EventQueue::new();
        let result = q.replay();
        assert!(matches!(
            result,
            Err(super::super::error::EventQueueError::ReplayNotEnabled)
        ));
    }

    #[test]
    fn test_replay_with_empty_log_returns_error() {
        let mut q = EventQueue::with_replay_log();
        let result = q.replay();
        assert!(matches!(
            result,
            Err(super::super::error::EventQueueError::ReplayLogEmpty)
        ));
    }

    #[test]
    fn test_stats_track_operations() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        for i in 0..5 {
            q.push(make_event(&mut b, i * 1_000));
        }
        q.next();
        q.next();
        q.fast_forward_to(Timestamp::from_nanos(2_000));
        let stats = q.stats();
        assert_eq!(stats.total_pushed, 5);
        assert_eq!(stats.total_popped, 2);
        assert_eq!(stats.total_skipped, 1);
    }

    #[test]
    fn test_drain_until_returns_events_in_order() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        q.push(make_event(&mut b, 100));
        q.push(make_event(&mut b, 200));
        q.push(make_event(&mut b, 300));
        q.push(make_event(&mut b, 400));
        let drained = q.drain_until(Timestamp::from_nanos(250));
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].timestamp(), Timestamp::from_nanos(100));
        assert_eq!(drained[1].timestamp(), Timestamp::from_nanos(200));
        // current_time 不变
        assert_eq!(q.current_time(), Timestamp::from_nanos(0));
        // 剩余 300, 400
        assert_eq!(q.len(), 2);
    }

    // ─── 补充边界场景 ─────────────────────────────────

    /// 空队列 pop/peek 返回 None
    #[test]
    fn test_empty_queue_pop_peek() {
        let mut q = EventQueue::new();
        assert!(q.next().is_none(), "空队列 pop 返回 None");
        assert!(q.peek().is_none(), "空队列 peek 返回 None");
        assert_eq!(q.peek_time(), None, "空队列 peek_time 返回 None");
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    /// 极小时间戳（Unix 纪元）
    #[test]
    fn test_unix_epoch_timestamp() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let event = make_event(&mut b, 0);
        q.push(event);
        let popped = q.next().expect("应能弹出");
        assert_eq!(popped.timestamp(), Timestamp::from_nanos(0));
    }

    /// i64::MAX 时间戳
    #[test]
    fn test_max_timestamp_event() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        let event = make_event(&mut b, i64::MAX);
        q.push(event);
        let popped = q.next().expect("应能弹出");
        assert_eq!(popped.timestamp(), Timestamp::from_nanos(i64::MAX));
    }

    /// 同一时间戳不同 seq 应按 seq 升序弹出（FIFO）
    #[test]
    fn test_same_timestamp_seq_ordering() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 同一时间戳 t=1000, seq 3 个事件
        let e1 = make_event(&mut b, 1000); // seq=0
        let e2 = make_event(&mut b, 1000); // seq=1
        let e3 = make_event(&mut b, 1000); // seq=2
        q.push(e1);
        q.push(e2);
        q.push(e3);

        let p1 = q.next().unwrap();
        let p2 = q.next().unwrap();
        let p3 = q.next().unwrap();
        // seq 顺序：0 < 1 < 2
        assert!(p1.seq() < p2.seq());
        assert!(p2.seq() < p3.seq());
    }

    /// 时间戳乱序入队：弹出顺序仍按时间戳排序
    #[test]
    fn test_out_of_order_push_orders_by_time() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 乱序 push
        q.push(make_event(&mut b, 300));
        q.push(make_event(&mut b, 100));
        q.push(make_event(&mut b, 200));

        // 弹出顺序应为 100, 200, 300
        assert_eq!(q.next().unwrap().timestamp(), Timestamp::from_nanos(100));
        assert_eq!(q.next().unwrap().timestamp(), Timestamp::from_nanos(200));
        assert_eq!(q.next().unwrap().timestamp(), Timestamp::from_nanos(300));
        assert!(q.is_empty());
    }

    /// 大量事件（10K）批量入队与弹出
    #[test]
    fn test_large_batch_push_pop() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        // 乱序 push 10K 事件
        for i in (0..10_000).rev() {
            q.push(make_event(&mut b, i as i64));
        }
        assert_eq!(q.len(), 10_000);

        // 弹出顺序应严格递增
        let mut prev = -1_i64;
        for _ in 0..10_000 {
            let evt = q.next().expect("应有事件");
            let ts = evt.timestamp().nanos;
            assert!(ts > prev, "时间戳非递增: {prev} -> {ts}");
            prev = ts;
        }
        assert!(q.is_empty());
    }

    /// drain_until 处理全部事件（无上限）
    #[test]
    fn test_drain_until_all() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        for i in 0..5 {
            q.push(make_event(&mut b, i * 100));
        }
        let drained = q.drain_until(Timestamp::from_nanos(i64::MAX));
        assert_eq!(drained.len(), 5);
        assert!(q.is_empty());
    }

    /// fast_forward_to 跳到中间时间
    #[test]
    fn test_fast_forward_to_middle() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        for i in 0..10 {
            q.push(make_event(&mut b, i * 100));
        }
        q.fast_forward_to(Timestamp::from_nanos(350));
        // current_time 应为 350
        assert_eq!(q.current_time(), Timestamp::from_nanos(350));
        // 剩余 [400, 500, ..., 900]
        assert_eq!(q.len(), 6);
    }

    /// reset 后队列为空但 replay 可恢复（如果启用日志）
    #[test]
    fn test_reset_clears_completely() {
        let mut q = EventQueue::with_replay_log();
        let mut b = EventBuilder::new(0);
        for i in 0..3 {
            q.push(make_event(&mut b, i * 100));
        }
        assert_eq!(q.len(), 3);
        q.reset();
        assert_eq!(q.len(), 0);
        assert!(q.is_empty());
        // replay log 仍在（可恢复）
        assert!(!q.replay_log().is_empty());
    }

    /// fast_forward_collect 返回跳过的所有事件
    #[test]
    fn test_fast_forward_collect_returns_skipped() {
        let mut q = EventQueue::new();
        let mut b = EventBuilder::new(0);
        for i in 0..5 {
            q.push(make_event(&mut b, i * 100));
        }
        // 事件时间戳：[0, 100, 200, 300, 400]
        // target=300，<= 300 的事件全部收集
        let collected = q.fast_forward_collect(Timestamp::from_nanos(300));
        assert_eq!(collected.len(), 4); // [0, 100, 200, 300]
        assert_eq!(q.len(), 1); // [400] 剩余
        assert_eq!(q.current_time(), Timestamp::from_nanos(300));
    }
}
