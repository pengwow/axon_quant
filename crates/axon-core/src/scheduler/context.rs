//! 任务回调上下文
//!
//! 0.8.0 改:用 `Arc<Mutex<EventQueue>>` 替代 `*mut EventQueue`,
//! 消除 5 处 unsafe(参见 `Scheduler` 模块说明)。SchedulerContext
//! 通过 `Arc<Mutex<>>` 跨 callback 安全共享事件队列,无裸指针。
//!
//! API 破坏性:`with_event_queue` / `event_queue_mut` 签名变(从
//! `&mut EventQueue` / `&mut EventQueue` 改为 `Arc<Mutex<EventQueue>>` /
//! `MutexGuard<EventQueue>`)。无外部调用者(0.7.1 验证),axon-core 内部
//! self-contained,影响范围限定在 `Scheduler::run_until` / `Scheduler::tick`
//! 及两个模块的测试。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::queue::EventQueue;
use crate::time::Timestamp;

/// 调度器上下文:传递给任务回调的共享状态
///
/// 持有 `Arc<Mutex<EventQueue>>` 以便任务向队列注入事件。
/// `Arc<Mutex<>>` 自带 `Send + Sync`,无需手动 `unsafe impl`。
pub struct SchedulerContext {
    /// 当前模拟时间
    pub current_time: Timestamp,
    /// 事件队列(0.8.0 改:`*mut` → `Arc<Mutex<>>`)
    pub event_queue: Option<Arc<Mutex<EventQueue>>>,
    /// 用户自定义状态
    pub user_data: HashMap<String, String>,
}

impl SchedulerContext {
    /// 创建新上下文(不绑定事件队列)
    pub fn new(current_time: Timestamp) -> Self {
        Self {
            current_time,
            event_queue: None,
            user_data: HashMap::new(),
        }
    }

    /// 绑定事件队列
    ///
    /// 0.8.0 改:接受 `Arc<Mutex<EventQueue>>` 而非 `&mut EventQueue`,
    /// 与 `Scheduler::run_until` / `tick` 新签名一致。
    pub fn with_event_queue(mut self, queue: Arc<Mutex<EventQueue>>) -> Self {
        self.event_queue = Some(queue);
        self
    }

    /// 访问事件队列,获取 `MutexGuard`
    ///
    /// 0.8.0 改:返回 `MutexGuard<EventQueue>`,Rust 借用检查器
    /// 会在 guard 生命周期内阻止其他借用。
    ///
    /// 返回 `None` 表示未绑定事件队列。
    pub fn event_queue_lock(&mut self) -> Option<MutexGuard<'_, EventQueue>> {
        self.event_queue
            .as_ref()
            .map(|arc| arc.lock().expect("EventQueue Mutex poisoned"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_new() {
        let ctx = SchedulerContext::new(Timestamp::from_nanos(1_000));
        assert_eq!(ctx.current_time, Timestamp::from_nanos(1_000));
        assert!(ctx.user_data.is_empty());
        assert!(ctx.event_queue.is_none());
    }

    #[test]
    fn test_user_data_mutation() {
        let mut ctx = SchedulerContext::new(Timestamp::from_nanos(0));
        ctx.user_data.insert("key".into(), "value".into());
        assert_eq!(ctx.user_data.get("key").map(|s| s.as_str()), Some("value"));
    }

    #[test]
    fn test_with_event_queue() {
        let q = Arc::new(Mutex::new(EventQueue::new()));
        let ctx = SchedulerContext::new(Timestamp::from_nanos(0)).with_event_queue(Arc::clone(&q));
        assert!(ctx.event_queue.is_some());
    }

    #[test]
    fn test_event_queue_lock_returns_guard() {
        let q = Arc::new(Mutex::new(EventQueue::new()));
        let mut ctx =
            SchedulerContext::new(Timestamp::from_nanos(0)).with_event_queue(Arc::clone(&q));
        let guard = ctx.event_queue_lock();
        assert!(guard.is_some());
    }
}
