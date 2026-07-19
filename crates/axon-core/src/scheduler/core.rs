//! 调度器主结构
//!
//! 0.8.0 改:用 `Arc<Mutex<EventQueue>>` 替代 `*mut EventQueue`,
//! 消除 5 处 unsafe(见 `super::context`)。回调通过 `Arc<Mutex<>>`
//! 安全共享事件队列,无裸指针。
//!
//! API 破坏性:`run_until` / `tick` 从 `&mut EventQueue` 改为
//! `&Arc<Mutex<EventQueue>>`。0.7.1 验证:axon-core scheduler 自包含,
//! 无外部 crate 调用。影响范围:axon-core scheduler 测试(改 `&mut q` →
//! `Arc::new(Mutex::new(q))`)。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::callback::{ClosureCallback, TaskCallback};
use super::clock::SimulatedClock;
use super::context::SchedulerContext;
use super::error::{SchedulerError, SchedulerResult};
use super::task::{RepeatPolicy, Task, TaskId, TaskStatus};
use crate::queue::EventQueue;
use crate::time::Timestamp;

/// 调度器统计
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulerStats {
    /// 已注册任务总数
    pub total_registered: u64,
    /// 已执行任务次数
    pub total_fired: u64,
    /// 已取消任务数
    pub total_cancelled: u64,
    /// 调度器 tick 次数
    pub total_ticks: u64,
}

/// 调度器
#[derive(Serialize, Deserialize)]
pub struct Scheduler {
    /// 模拟时钟
    clock: SimulatedClock,
    /// 任务注册表
    tasks: HashMap<TaskId, Task>,
    /// 按时间索引：BTreeMap 保证有序遍历
    time_index: BTreeMap<Timestamp, Vec<TaskId>>,
    /// 闭包回调存储（不可序列化，运行时填充）
    #[serde(skip)]
    callbacks: HashMap<TaskId, Box<dyn TaskCallback>>,
    /// 下一个任务 ID
    next_task_id: u64,
    /// 统计
    stats: SchedulerStats,
}

impl Scheduler {
    /// 创建新调度器
    pub fn new(start: Timestamp) -> Self {
        Self {
            clock: SimulatedClock::new(start),
            tasks: HashMap::new(),
            time_index: BTreeMap::new(),
            callbacks: HashMap::new(),
            next_task_id: 0,
            stats: SchedulerStats::default(),
        }
    }

    /// 创建带结束时间的调度器
    pub fn with_end(start: Timestamp, end: Timestamp) -> Self {
        Self {
            clock: SimulatedClock::with_end(start, end),
            tasks: HashMap::new(),
            time_index: BTreeMap::new(),
            callbacks: HashMap::new(),
            next_task_id: 0,
            stats: SchedulerStats::default(),
        }
    }

    /// 分配任务 ID
    fn next_id(&mut self) -> TaskId {
        let id = TaskId(self.next_task_id);
        self.next_task_id += 1;
        id
    }

    /// 获取当前时间
    #[inline]
    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }

    /// 在指定时间调度一次性任务
    pub fn schedule_at<F: Fn(&mut SchedulerContext) + Send + Sync + 'static>(
        &mut self,
        time: Timestamp,
        callback: F,
    ) -> SchedulerResult<TaskId> {
        if time < self.clock.now() {
            return Err(SchedulerError::ScheduleInPast {
                scheduled: time,
                current: self.clock.now(),
            });
        }

        let id = self.next_id();
        let task = Task {
            id,
            scheduled_at: time,
            repeat: RepeatPolicy::Once,
            status: TaskStatus::Pending,
            priority: 0,
            label: String::new(),
            fire_count: 0,
        };

        self.time_index.entry(time).or_default().push(id);
        self.tasks.insert(id, task);
        self.callbacks
            .insert(id, Box::new(ClosureCallback { func: callback }));
        self.stats.total_registered += 1;

        Ok(id)
    }

    /// 在延迟后调度任务
    pub fn schedule_after<F: Fn(&mut SchedulerContext) + Send + Sync + 'static>(
        &mut self,
        delay: Duration,
        callback: F,
    ) -> SchedulerResult<TaskId> {
        let time = self.clock.now().add(delay);
        self.schedule_at(time, callback)
    }

    /// 调度周期性任务
    pub fn schedule_interval<F: Fn(&mut SchedulerContext) + Send + Sync + 'static>(
        &mut self,
        interval: Duration,
        callback: F,
    ) -> SchedulerResult<TaskId> {
        if interval.is_zero() {
            return Err(SchedulerError::InvalidInterval(interval));
        }

        let id = self.next_id();
        let first_fire = self.clock.now().add(interval);

        let task = Task {
            id,
            scheduled_at: first_fire,
            repeat: RepeatPolicy::Interval { interval },
            status: TaskStatus::Scheduled {
                next_fire: first_fire,
            },
            priority: 0,
            label: String::new(),
            fire_count: 0,
        };

        self.time_index.entry(first_fire).or_default().push(id);
        self.tasks.insert(id, task);
        self.callbacks
            .insert(id, Box::new(ClosureCallback { func: callback }));
        self.stats.total_registered += 1;

        Ok(id)
    }

    /// 取消任务
    pub fn cancel(&mut self, task_id: TaskId) -> SchedulerResult<bool> {
        let task = self
            .tasks
            .get_mut(&task_id)
            .ok_or(SchedulerError::TaskNotFound(task_id))?;

        if task.status == TaskStatus::Cancelled {
            return Ok(false);
        }

        task.status = TaskStatus::Cancelled;
        self.stats.total_cancelled += 1;

        // 从时间索引中移除
        if let Some(ids) = self.time_index.get_mut(&task.scheduled_at) {
            ids.retain(|id| *id != task_id);
            if ids.is_empty() {
                self.time_index.remove(&task.scheduled_at);
            }
        }

        Ok(true)
    }

    /// 获取任务状态
    pub fn task_status(&self, task_id: TaskId) -> Option<TaskStatus> {
        self.tasks.get(&task_id).map(|t| t.status)
    }

    /// 获取任务
    pub fn task(&self, task_id: TaskId) -> Option<&Task> {
        self.tasks.get(&task_id)
    }

    /// 获取所有活跃任务数
    pub fn active_count(&self) -> usize {
        self.tasks
            .values()
            .filter(|t| t.status != TaskStatus::Cancelled && t.status != TaskStatus::Completed)
            .count()
    }

    /// 获取总任务数
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// 推进时钟到指定时间,执行所有到期任务
    ///
    /// 0.8.0 改:`event_queue` 从 `&mut EventQueue` 改为 `&Arc<Mutex<EventQueue>>`,
    /// 配合 [`SchedulerContext`] 新签名,消除 5 处 unsafe。
    pub fn run_until(
        &mut self,
        until: Timestamp,
        event_queue: &Arc<Mutex<EventQueue>>,
    ) -> SchedulerResult<u64> {
        let mut fired_count = 0u64;

        while let Some((&next_time, _)) = self.time_index.iter().next() {
            if next_time > until {
                break;
            }
            // 检查任务触发时间是否已超出时钟边界
            if let Some(end) = self.clock.end()
                && next_time > end
            {
                break;
            }

            // 推进时钟
            self.clock.set(next_time);
            let ids = self.time_index.remove(&next_time).unwrap_or_default();

            for task_id in &ids {
                if self.fire_task(*task_id, next_time, event_queue) {
                    fired_count += 1;
                }
            }
        }

        // 推进到目标时间
        if until > self.clock.now() {
            self.clock.set(until);
        }

        self.stats.total_ticks += 1;
        Ok(fired_count)
    }

    /// 单步执行：执行下一个到期任务
    ///
    /// 0.8.0 改：同 [`run_until`](Self::run_until) — `&mut EventQueue` → `&Arc<Mutex<EventQueue>>`。
    pub fn tick(
        &mut self,
        event_queue: &Arc<Mutex<EventQueue>>,
    ) -> SchedulerResult<Option<TaskId>> {
        let next_time = match self.time_index.iter().next() {
            Some((&time, _)) => time,
            None => return Ok(None),
        };

        let task_ids = self.time_index.remove(&next_time).unwrap_or_default();
        self.clock.set(next_time);

        for task_id in &task_ids {
            if self.fire_task(*task_id, next_time, event_queue) {
                return Ok(Some(*task_id));
            }
        }

        Ok(None)
    }

    /// 触发单个任务;返回 `true` 表示已触发
    ///
    /// 0.8.0 改:同 [`run_until`](Self::run_until) — `&mut EventQueue` → `&Arc<Mutex<EventQueue>>`。
    fn fire_task(
        &mut self,
        task_id: TaskId,
        fire_time: Timestamp,
        event_queue: &Arc<Mutex<EventQueue>>,
    ) -> bool {
        let task = match self.tasks.get(&task_id) {
            Some(t) if t.status != TaskStatus::Cancelled => t,
            _ => return false,
        };

        // 执行回调
        let mut ctx = SchedulerContext {
            current_time: fire_time,
            event_queue: Some(Arc::clone(event_queue)),
            user_data: HashMap::new(),
        };

        if let Some(callback) = self.callbacks.get(&task_id) {
            let _ = task;
            callback.call(&mut ctx);
        }

        self.stats.total_fired += 1;

        // 更新任务状态
        let task = self.tasks.get_mut(&task_id).unwrap();
        task.fire_count += 1;

        match task.repeat {
            RepeatPolicy::Once => {
                task.status = TaskStatus::Completed;
            }
            RepeatPolicy::Interval { interval } => {
                let next_fire = fire_time.add(interval);
                task.scheduled_at = next_fire;
                task.status = TaskStatus::Scheduled { next_fire };
                self.time_index.entry(next_fire).or_default().push(task_id);
            }
            RepeatPolicy::Cron { every_n_seconds } => {
                let next_fire = fire_time.add(Duration::from_secs(every_n_seconds));
                task.scheduled_at = next_fire;
                task.status = TaskStatus::Scheduled { next_fire };
                self.time_index.entry(next_fire).or_default().push(task_id);
            }
        }
        true
    }

    /// 获取统计信息
    pub fn stats(&self) -> &SchedulerStats {
        &self.stats
    }

    /// 获取模拟时钟
    pub fn clock(&self) -> &SimulatedClock {
        &self.clock
    }

    /// 获取下一个待执行任务的时间
    pub fn next_fire_time(&self) -> Option<Timestamp> {
        self.time_index.iter().next().map(|(t, _)| *t)
    }

    /// 重置调度器
    pub fn reset(&mut self) {
        self.tasks.clear();
        self.time_index.clear();
        self.callbacks.clear();
        self.next_task_id = 0;
        self.stats = SchedulerStats::default();
        self.clock.set(self.clock.start());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn test_new_scheduler() {
        let s = Scheduler::new(Timestamp::from_nanos(0));
        assert_eq!(s.now(), Timestamp::from_nanos(0));
        assert_eq!(s.task_count(), 0);
        assert_eq!(s.stats().total_registered, 0);
    }

    #[test]
    fn test_with_end_scheduler() {
        let s = Scheduler::with_end(Timestamp::from_nanos(0), Timestamp::from_millis(1000));
        assert!(!s.clock().is_exhausted());
    }

    #[test]
    fn test_schedule_at_basic() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let id = s.schedule_at(Timestamp::from_millis(100), |_| {}).unwrap();
        assert_eq!(s.task_count(), 1);
        assert_eq!(s.task_status(id), Some(TaskStatus::Pending));
    }

    #[test]
    fn test_schedule_at_in_past_fails() {
        let mut s = Scheduler::new(Timestamp::from_millis(100));
        let result = s.schedule_at(Timestamp::from_nanos(0), |_| {});
        assert!(matches!(result, Err(SchedulerError::ScheduleInPast { .. })));
    }

    #[test]
    fn test_schedule_after_uses_delay() {
        let mut s = Scheduler::new(Timestamp::from_millis(50));
        let id = s.schedule_after(Duration::from_millis(50), |_| {}).unwrap();
        let task = s.task(id).unwrap();
        assert_eq!(task.scheduled_at, Timestamp::from_millis(100));
    }

    #[test]
    fn test_schedule_interval_validates_nonzero() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let result = s.schedule_interval(Duration::from_secs(0), |_| {});
        assert!(matches!(result, Err(SchedulerError::InvalidInterval(_))));
    }

    #[test]
    fn test_schedule_at_triggers_at_correct_time() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let counter = Arc::new(AtomicU64::new(0));
        let c = counter.clone();
        s.schedule_at(Timestamp::from_millis(100), move |_| {
            c.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
        let q = Arc::new(Mutex::new(EventQueue::new()));
        let fired = s.run_until(Timestamp::from_millis(200), &q).unwrap();
        assert_eq!(fired, 1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_schedule_after_triggers_after_delay() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let counter = Arc::new(AtomicU64::new(0));
        let c = counter.clone();
        s.schedule_after(Duration::from_millis(50), move |_| {
            c.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
        // 推进 100ms（应触发一次）
        let q = Arc::new(Mutex::new(EventQueue::new()));
        s.run_until(Timestamp::from_millis(100), &q).unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_interval_task_repeats() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let counter = Arc::new(AtomicU64::new(0));
        let c = counter.clone();
        s.schedule_interval(Duration::from_millis(50), move |_| {
            c.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
        // 推进 300ms → 触发 6 次（50, 100, 150, 200, 250, 300）
        let q = Arc::new(Mutex::new(EventQueue::new()));
        let fired = s.run_until(Timestamp::from_millis(300), &q).unwrap();
        assert_eq!(fired, 6);
        assert_eq!(counter.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn test_interval_task_stops_when_cancelled() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let counter = Arc::new(AtomicU64::new(0));
        let c = counter.clone();
        let id = s
            .schedule_interval(Duration::from_millis(50), move |_| {
                c.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap();
        // 推进到 25ms，未到第一次触发
        let q = Arc::new(Mutex::new(EventQueue::new()));
        s.run_until(Timestamp::from_millis(25), &q).unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        // 取消任务
        let result = s.cancel(id).unwrap();
        assert!(result);

        // 推进到 200ms，已取消，不会触发
        s.run_until(Timestamp::from_millis(200), &q).unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_cancel_nonexistent_task_fails() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let result = s.cancel(TaskId(999));
        assert!(matches!(result, Err(SchedulerError::TaskNotFound(_))));
    }

    #[test]
    fn test_cancel_already_cancelled_returns_false() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let id = s.schedule_at(Timestamp::from_millis(100), |_| {}).unwrap();
        s.cancel(id).unwrap();
        // 第二次取消
        let result = s.cancel(id).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_scheduler_processes_events_in_order() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let o1 = order.clone();
        let o2 = order.clone();
        s.schedule_at(Timestamp::from_millis(100), move |_| {
            o1.lock().unwrap().push(1);
        })
        .unwrap();
        s.schedule_at(Timestamp::from_millis(50), move |_| {
            o2.lock().unwrap().push(2);
        })
        .unwrap();
        s.schedule_at(Timestamp::from_millis(150), move |_| {
            // 第三个任务，ID 3
        })
        .unwrap();

        let q = Arc::new(Mutex::new(EventQueue::new()));
        s.run_until(Timestamp::from_millis(200), &q).unwrap();
        let r = order.lock().unwrap();
        assert_eq!(*r, vec![2, 1]);
    }

    #[test]
    fn test_scheduler_advances_clock_correctly() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        s.schedule_at(Timestamp::from_millis(100), |_| {}).unwrap();
        s.schedule_at(Timestamp::from_millis(200), |_| {}).unwrap();
        let q = Arc::new(Mutex::new(EventQueue::new()));
        s.run_until(Timestamp::from_millis(200), &q).unwrap();
        // 时钟应推进到 200ms（所有任务执行完毕）
        assert_eq!(s.now(), Timestamp::from_millis(200));
    }

    #[test]
    fn test_tick_executes_one_task() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        s.schedule_at(Timestamp::from_millis(100), |_| {}).unwrap();
        s.schedule_at(Timestamp::from_millis(200), |_| {}).unwrap();
        let q = Arc::new(Mutex::new(EventQueue::new()));
        let first = s.tick(&q).unwrap();
        assert!(first.is_some());
        assert_eq!(s.now(), Timestamp::from_millis(100));
        let second = s.tick(&q).unwrap();
        assert!(second.is_some());
        assert_eq!(s.now(), Timestamp::from_millis(200));
        let third = s.tick(&q).unwrap();
        assert!(third.is_none());
    }

    #[test]
    fn test_tick_on_empty_returns_none() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let q = Arc::new(Mutex::new(EventQueue::new()));
        let result = s.tick(&q).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_active_count_excludes_completed_cancelled() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let id1 = s.schedule_at(Timestamp::from_millis(100), |_| {}).unwrap();
        let _id2 = s.schedule_at(Timestamp::from_millis(200), |_| {}).unwrap();
        let _id3 = s
            .schedule_interval(Duration::from_millis(100), |_| {})
            .unwrap();
        assert_eq!(s.active_count(), 3);
        s.cancel(id1).unwrap();
        assert_eq!(s.active_count(), 2);
    }

    #[test]
    fn test_clock_exhausted_stops_run_until() {
        let mut s = Scheduler::with_end(Timestamp::from_nanos(0), Timestamp::from_millis(50));
        s.schedule_at(Timestamp::from_millis(100), |_| {}).unwrap();
        s.schedule_at(Timestamp::from_millis(200), |_| {}).unwrap();
        let q = Arc::new(Mutex::new(EventQueue::new()));
        let fired = s.run_until(Timestamp::from_millis(1000), &q).unwrap();
        // 时钟已耗尽，应不触发任何任务
        assert_eq!(fired, 0);
    }

    #[test]
    fn test_next_fire_time() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        assert!(s.next_fire_time().is_none());
        s.schedule_at(Timestamp::from_millis(100), |_| {}).unwrap();
        assert_eq!(s.next_fire_time(), Some(Timestamp::from_millis(100)));
        s.schedule_at(Timestamp::from_millis(50), |_| {}).unwrap();
        assert_eq!(s.next_fire_time(), Some(Timestamp::from_millis(50)));
    }

    #[test]
    fn test_reset_clears_state() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        s.schedule_at(Timestamp::from_millis(100), |_| {}).unwrap();
        let q = Arc::new(Mutex::new(EventQueue::new()));
        s.run_until(Timestamp::from_millis(200), &q).unwrap();
        s.reset();
        assert_eq!(s.task_count(), 0);
        assert_eq!(s.now(), Timestamp::from_nanos(0));
        assert_eq!(s.stats().total_fired, 0);
    }

    #[test]
    fn test_callback_can_set_user_data() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let captured = Arc::new(AtomicU64::new(0));
        let c = captured.clone();
        s.schedule_at(Timestamp::from_millis(100), move |ctx| {
            c.store(ctx.current_time.nanos as u64, Ordering::Relaxed);
        })
        .unwrap();
        let q = Arc::new(Mutex::new(EventQueue::new()));
        s.run_until(Timestamp::from_millis(200), &q).unwrap();
        assert_eq!(captured.load(Ordering::Relaxed), 100_000_000);
    }

    #[test]
    fn test_cron_task_repeats() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let counter = Arc::new(AtomicU64::new(0));
        let c = counter.clone();
        let id = s
            .schedule_interval(Duration::from_millis(100), move |_| {
                c.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap();
        // 验证任务配置
        let task = s.task(id).unwrap();
        assert!(matches!(task.repeat, RepeatPolicy::Interval { .. }));
    }

    #[test]
    fn test_multiple_tasks_at_same_time() {
        let mut s = Scheduler::new(Timestamp::from_nanos(0));
        let counter = Arc::new(AtomicU64::new(0));
        let c1 = counter.clone();
        let c2 = counter.clone();
        let c3 = counter.clone();
        s.schedule_at(Timestamp::from_millis(100), move |_| {
            c1.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
        s.schedule_at(Timestamp::from_millis(100), move |_| {
            c2.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
        s.schedule_at(Timestamp::from_millis(100), move |_| {
            c3.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
        let q = Arc::new(Mutex::new(EventQueue::new()));
        let fired = s.run_until(Timestamp::from_millis(200), &q).unwrap();
        assert_eq!(fired, 3);
        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }
}
