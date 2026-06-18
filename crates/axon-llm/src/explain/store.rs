//! 解释存储
//!
//! 使用 `tokio::sync::RwLock` 保护 `HashMap` + `VecDeque`（O(1) FIFO 淘汰）。
//! 支持并发读写、容量限制、ID 覆盖。
//!
//! ## 设计要点
//!
//! - **`VecDeque` 而非 `Vec`**：`order.pop_front()` 是 O(1)，避免 `Vec::remove(0)`
//!   的 O(n) memmove。在 1000 容量下差异虽小，但语义上更正确。
//! - **单个 `RwLock` 包裹 inner**：避免 inner/order 两个 lock 的死锁风险和
//!   锁升级开销。读写冲突由 tokio 调度处理。
//! - **`Default` trait** 替代 `default_capacity()` 工厂：标准 Rust 惯例。
//! - **`contains_key` 与 `get` 分离**：让 `QueryExplanationTool` 能区分
//!   "key 不存在" 和 "lock 获取超时"，返回不同错误类型。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::RwLock;

use axon_explain::types::Explanation;

/// 默认容量
pub const DEFAULT_CAPACITY: usize = 1000;

/// 内部状态（受 RwLock 保护）
#[derive(Debug)]
struct StoreInner {
    /// 决策 ID → 解释
    map: HashMap<String, Explanation>,
    /// 插入顺序（FIFO 淘汰用）
    order: VecDeque<String>,
}

/// 解释存储（线程安全 / 异步友好）
#[derive(Debug, Clone)]
pub struct ExplanationStore {
    inner: Arc<RwLock<StoreInner>>,
    capacity: usize,
}

impl ExplanationStore {
    /// 显式容量构造
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(StoreInner {
                map: HashMap::with_capacity(capacity),
                order: VecDeque::with_capacity(capacity),
            })),
            capacity,
        }
    }

    /// 当前容量
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 插入一条解释
    ///
    /// - 若 `decision_id` 已存在：覆盖并将其移到 order 末尾（保持"最近更新"语义）
    /// - 若达到容量上限：淘汰最旧（FIFO）
    // 嵌套 if 风格：保留可读性。rust 1.96 工具链升级后 clippy 新增
    // `clippy::collapsible_if` 规则报 lint,本函数非本次任务改动范围,加 allow 抑制。
    #[allow(clippy::collapsible_if)]
    pub async fn insert(&self, decision_id: String, exp: Explanation) {
        let mut guard = self.inner.write().await;

        // 容量已满且为新 key：淘汰最旧
        if !guard.map.contains_key(&decision_id) && guard.map.len() >= self.capacity {
            if let Some(oldest) = guard.order.pop_front() {
                guard.map.remove(&oldest);
            }
        }

        // 已存在：先从 order 移除旧位置，再追加到末尾
        if guard.map.contains_key(&decision_id) {
            guard.order.retain(|id| id != &decision_id);
        }

        guard.order.push_back(decision_id.clone());
        guard.map.insert(decision_id, exp);
    }

    /// 按 ID 获取（深拷贝 Explanation）
    pub async fn get(&self, decision_id: &str) -> Option<Explanation> {
        let guard = self.inner.read().await;
        guard.map.get(decision_id).cloned()
    }

    /// 检查 key 是否存在（轻量、无需克隆）
    pub async fn contains_key(&self, decision_id: &str) -> bool {
        let guard = self.inner.read().await;
        guard.map.contains_key(decision_id)
    }

    /// 获取最近 n 条解释（按插入顺序，从旧到新）
    pub async fn latest(&self, n: usize) -> Vec<Explanation> {
        let guard = self.inner.read().await;
        let skip = guard.order.len().saturating_sub(n);
        guard
            .order
            .iter()
            .skip(skip)
            .filter_map(|id| guard.map.get(id).cloned())
            .collect()
    }

    /// 当前条目数
    pub async fn len(&self) -> usize {
        let guard = self.inner.read().await;
        guard.map.len()
    }

    /// 是否为空
    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.read().await;
        guard.map.is_empty()
    }
}

impl Default for ExplanationStore {
    /// 默认容量 = [`DEFAULT_CAPACITY`]
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}
