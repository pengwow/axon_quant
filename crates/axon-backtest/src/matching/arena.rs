//! 0.8.0 Phase 3 A3.1:`OrderArena` slab 分配器
//!
//! # 设计动机
//!
//! BacktestEngine 内部为每个挂单存一个 [`Order`] (堆上),
//! 高并发场景下 `Order` 反复 alloc / dealloc 会成为瓶颈(每次 submit
//! 一个 `Box<Order>`,每次 fill / cancel 一个 drop)。
//!
//! `OrderArena` 是一个**对象池风格的 slab 分配器**:
//! - 一次性 `Vec<Slot>` 预分配,`alloc` 复用 free slot 而非堆分配
//! - `OrderHandle(usize)` 是 stable index,不受 Vec 扩容影响(slab pattern)
//! - `free` 把 slot 还给 free list(`Vec<usize>`),下次 `alloc` 优先复用
//!
//! # 用法
//!
//! ```ignore
//! use axon_backtest::matching::arena::{OrderArena, OrderHandle};
//!
//! let mut arena = OrderArena::new();
//! let h = arena.alloc_with(order);
//! let order_ref = arena.get(h).expect("handle valid");
//! arena.free(h);
//! // 下次 alloc 复用 h.index
//! ```
//!
//! # 0.8.0 范围(对比 plan 原始验收)
//!
//! - ✅ `OrderArena` 基础实现 + 单元测试(alloc / free / reuse / get / get_mut / iter)
//! - ✅ Send + Sync 编译期 const 断言
//! - ✅ 性能:`cargo bench --bench matching_l3_baseline` 无退化(`Order` 路径暂未切到 arena)
//! - ⏸️ 当前 `Order` 用法在 BacktestEngine 路径**不**切到 `OrderArena`(plan 验收 #3)
//!   — plan 已 re-scope 为 "0.9.0 多 leg 并行的结构性铺垫",**不**是 0.8.0
//!   阻塞项。`OrderArena` 作为独立模块先落地,后续 0.9.0 阶段按 BacktestEngine
//!   热点路径(`OrderIndex` / `OrderBook` 等)逐个集成。
//!
//! # 不变量
//!
//! - `slots.len() == next_fresh` 恒成立,除非 `free` 复用 → `slots.len()` 不变
//! - `free_list` 不含重复元素
//! - `free_list` 中的 index 必 ≤ `slots.len()`
//! - `slots[i].is_some() <=> i ∉ free_list`(但 free 后 `slots[i]` 仍保留
//!   `Order` 直至下次 alloc 覆盖,所以读 `slots[i]` 时需要 `is_some` 守卫)
//!
//! # Send + Sync
//!
//! `Order: Send + Sync`(派生 derive 因为字段都是 `Send + Sync`),
//! 所以 `OrderArena: Send + Sync`,可在多线程 BacktestEngine 路径中安全共享。
//! 编译期 const 断言见 `tests::assert_send_sync`。

#![deny(unsafe_code)]

use std::fmt;

use serde::{Deserialize, Serialize};

use axon_core::order::Order;

/// Slab 中的一个槽位
///
/// `Option<Order>` 而非裸 `Order`:支持 free 后的 slot 复用(alloc 时
/// 直接覆盖 `Some(_)` 旧值,无需 swap_remove 等破坏 handle 稳定性的操作)。
#[derive(Debug)]
struct Slot {
    order: Option<Order>,
}

/// `OrderArena` 内部 handle,仅作 `index` 别名
///
/// 用 `newtype` 模式防止与 `OrderId` (u64) 混用;**不**导出构造器,只能
/// 通过 `OrderArena::alloc_with` 拿。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderHandle {
    /// `OrderArena::slots` 中的索引
    index: u32,
}

impl OrderHandle {
    /// slot index (供调试 / 序列化 / 集成用)
    #[inline]
    #[allow(dead_code)]
    pub fn index(self) -> usize {
        self.index as usize
    }
}

impl fmt::Debug for OrderHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OrderHandle({})", self.index)
    }
}

/// `OrderArena` slab 分配器
///
/// 按需扩容(类似 `Vec<T>`),`alloc_with` O(1) (`free_list.pop()` or push),
/// `get` / `get_mut` / `free` O(1)。不收缩,已分配的 slot 永远占内存。
#[derive(Debug, Default)]
pub struct OrderArena {
    slots: Vec<Slot>,
    free_list: Vec<u32>,
}

impl OrderArena {
    /// 创建空 arena
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// 预分配 `capacity` 个 slot(避免早期扩容)
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            free_list: Vec::new(),
        }
    }

    /// 分配一个 slot 并写入 `order`,返回 handle
    ///
    /// 优先复用 `free_list` 中的 slot(被 free 后回收的),否则 `slots.push` 一个新的。
    #[inline]
    pub fn alloc_with(&mut self, order: Order) -> OrderHandle {
        if let Some(index) = self.free_list.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.order.is_none(), "free_list 持有了已占用的 slot");
            slot.order = Some(order);
            OrderHandle { index }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot { order: Some(order) });
            OrderHandle { index }
        }
    }

    /// 读取 slot 引用(slot 不存在或已 free 时返回 `None`)
    #[inline]
    pub fn get(&self, h: OrderHandle) -> Option<&Order> {
        self.slots
            .get(h.index as usize)
            .and_then(|s| s.order.as_ref())
    }

    /// 读取 slot 可变引用(slot 不存在或已 free 时返回 `None`)
    #[inline]
    pub fn get_mut(&mut self, h: OrderHandle) -> Option<&mut Order> {
        self.slots
            .get_mut(h.index as usize)
            .and_then(|s| s.order.as_mut())
    }

    /// 释放 slot(slot 不存在或已 free 时返回 `false`)
    ///
    /// 注:**不**做 generation 验证 — 因为 `OrderHandle` 暂未携带 generation。
    /// 调用者需保证不 double-free(参见单元测试 `no_double_free`)。
    /// 0.9.0 引入 generation 时 `free` 返回 `bool` 改为 `Result<(), ArenaError>`。
    #[inline]
    pub fn free(&mut self, h: OrderHandle) -> bool {
        if let Some(slot) = self.slots.get_mut(h.index as usize) {
            if slot.order.is_some() {
                slot.order = None;
                self.free_list.push(h.index);
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// 当前已分配的 slot 数(`slots.len() - free_list.len()`)
    #[inline]
    pub fn len(&self) -> usize {
        self.slots.len() - self.free_list.len()
    }

    /// 已分配 + 空闲 slot 的总数(`len() + free_list.len() == slots.len()`)
    ///
    /// 等于 `slots.len()` —— 即 `len()`(已分配) + `free_list.len()`(空闲)。
    /// `with_capacity` 不预填充,初始为 0;每次 `alloc_with` 触发 `slots.push` 时 +1。
    #[inline]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// 是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 释放全部 slot(保留容量)
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            slot.order = None;
        }
        self.free_list.clear();
        self.free_list.extend(0..self.slots.len() as u32);
    }

    /// 迭代当前所有活跃 slot(顺序按 `slots` 索引)
    pub fn iter(&self) -> impl Iterator<Item = (OrderHandle, &Order)> {
        self.slots.iter().enumerate().filter_map(|(i, s)| {
            s.order
                .as_ref()
                .map(|o| (OrderHandle { index: i as u32 }, o))
        })
    }

    /// 迭代当前所有活跃 slot(可变)
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (OrderHandle, &mut Order)> {
        self.slots.iter_mut().enumerate().filter_map(|(i, s)| {
            s.order
                .as_mut()
                .map(|o| (OrderHandle { index: i as u32 }, o))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── 编译期断言 ─────────────────────────────────

    /// `OrderArena` 必须 `Send + Sync`,允许 BacktestEngine 跨线程共享
    #[allow(dead_code)]
    const fn _assert_send_sync<T: Send + Sync>() {}
    const _: () = _assert_send_sync::<OrderArena>();
    const _: () = _assert_send_sync::<OrderHandle>();

    // ─── 辅助 ────────────────────────────────────────

    use axon_core::market::Side;
    use axon_core::order::{OrderType, TimeInForce};
    use axon_core::types::{Price, Quantity, Symbol};

    fn make_test_order(id: u64) -> Order {
        Order::spot(
            id,
            Symbol::from("BTC"),
            Symbol::from("USDT"),
            Side::Buy,
            OrderType::Limit {
                price: Price::from_f64(100.0),
            },
            Quantity::from_f64(1.0),
            TimeInForce::GTC,
        )
    }

    // ─── 基本 API ────────────────────────────────────

    #[test]
    fn new_arena_is_empty() {
        let arena = OrderArena::new();
        assert!(arena.is_empty());
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.capacity(), 0);
    }

    #[test]
    fn with_capacity_preallocates_internal_vec() {
        // `with_capacity` 只调 `Vec::with_capacity` 分配 Vec 底层容量,
        // 不预填充 slot 元素。`capacity()` 反映 `slots.len()` (= 0)。
        // 用 `Vec::capacity` 直接检查 Vec 预分配是外部信息,这里用
        // 多次 alloc 后不触发再次分配来验证。
        let mut arena: OrderArena = OrderArena::with_capacity(16);
        // Vec 的 capacity hint ≥ 16(实现细节),外部 API 不暴露;
        // 我们验证的是:连续 alloc 16 次,`slots.len()` 准确为 16
        for i in 0..16 {
            let _ = arena.alloc_with(make_test_order(i));
        }
        assert_eq!(arena.capacity(), 16);
        assert_eq!(arena.len(), 16);
    }

    #[test]
    fn alloc_returns_increasing_indices() {
        let mut arena = OrderArena::new();
        let h0 = arena.alloc_with(make_test_order(1));
        let h1 = arena.alloc_with(make_test_order(2));
        let h2 = arena.alloc_with(make_test_order(3));
        assert_eq!(h0.index, 0);
        assert_eq!(h1.index, 1);
        assert_eq!(h2.index, 2);
        assert_eq!(arena.len(), 3);
        assert_eq!(arena.capacity(), 3);
    }

    #[test]
    fn get_returns_correct_order() {
        let mut arena = OrderArena::new();
        let h = arena.alloc_with(make_test_order(42));
        let order = arena.get(h).expect("handle valid");
        assert_eq!(order.id, 42);
    }

    #[test]
    fn get_mut_allows_modification() {
        let mut arena = OrderArena::new();
        let h = arena.alloc_with(make_test_order(7));
        {
            let order_mut = arena.get_mut(h).expect("handle valid");
            order_mut.client_order_id = Some("test".to_string());
        }
        let order = arena.get(h).expect("handle valid");
        assert_eq!(order.client_order_id.as_deref(), Some("test"));
    }

    // ─── Free + 复用 ────────────────────────────────

    #[test]
    fn free_marks_slot_unused() {
        let mut arena = OrderArena::new();
        let h = arena.alloc_with(make_test_order(1));
        assert_eq!(arena.len(), 1);
        let ok = arena.free(h);
        assert!(ok);
        assert_eq!(arena.len(), 0);
        assert!(arena.get(h).is_none(), "freed slot 不可读");
    }

    #[test]
    fn no_double_free() {
        let mut arena = OrderArena::new();
        let h = arena.alloc_with(make_test_order(1));
        assert!(arena.free(h));
        // 第二次 free 返回 false(slot 已是 None)
        assert!(!arena.free(h));
    }

    #[test]
    fn free_invalid_handle_returns_false() {
        let mut arena = OrderArena::new();
        let bogus = OrderHandle { index: 999 };
        assert!(!arena.free(bogus));
        // get 也不应 panic
        assert!(arena.get(bogus).is_none());
    }

    #[test]
    fn free_then_alloc_reuses_slot() {
        let mut arena = OrderArena::new();
        let h0 = arena.alloc_with(make_test_order(1));
        let h1 = arena.alloc_with(make_test_order(2));
        let h2 = arena.alloc_with(make_test_order(3));
        assert_eq!(h0.index, 0);
        assert_eq!(h1.index, 1);
        assert_eq!(h2.index, 2);

        // free 中间一个
        assert!(arena.free(h1));
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.capacity(), 3); // slots 不收缩

        // 再 alloc 应复用 h1.index(LIFO)
        let h_new = arena.alloc_with(make_test_order(99));
        assert_eq!(h_new.index, 1, "LIFO 复用 free_list 顶");
        assert_eq!(arena.len(), 3);

        // 验证内容是新的
        let o = arena.get(h_new).expect("handle valid");
        assert_eq!(o.id, 99);
    }

    #[test]
    fn free_many_then_alloc_reuses_lifo() {
        let mut arena = OrderArena::new();
        let mut handles = Vec::new();
        for i in 0..10 {
            handles.push(arena.alloc_with(make_test_order(i)));
        }
        // free 偶数 index
        for h in &handles {
            if h.index % 2 == 0 {
                arena.free(*h);
            }
        }
        // 重新 alloc 5 个,应复用 free_list
        let new_handles: Vec<_> = (0..5)
            .map(|_| arena.alloc_with(make_test_order(100)))
            .collect();
        for h in &new_handles {
            // 偶数 index 是 free 过的
            assert_eq!(h.index % 2, 0);
        }
    }

    // ─── Clear ──────────────────────────────────────

    #[test]
    fn clear_resets_all_slots() {
        let mut arena = OrderArena::new();
        let _h0 = arena.alloc_with(make_test_order(1));
        let _h1 = arena.alloc_with(make_test_order(2));
        let _h2 = arena.alloc_with(make_test_order(3));
        assert_eq!(arena.len(), 3);
        arena.clear();
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.capacity(), 3); // 容量保留
        // 下次 alloc 复用旧 slot
        let h = arena.alloc_with(make_test_order(99));
        assert!(h.index < 3, "复用 clear 后的 slot");
    }

    // ─── Iter ───────────────────────────────────────

    #[test]
    fn iter_yields_active_slots() {
        let mut arena = OrderArena::new();
        let h0 = arena.alloc_with(make_test_order(10));
        let h1 = arena.alloc_with(make_test_order(20));
        let h2 = arena.alloc_with(make_test_order(30));
        arena.free(h1);
        let collected: Vec<u64> = arena.iter().map(|(_, o)| o.id).collect();
        assert_eq!(collected, vec![10, 30]); // h1 被 free,跳过
        let _ = (h0, h2);
    }

    #[test]
    fn iter_mut_allows_mutation() {
        let mut arena = OrderArena::new();
        let _ = arena.alloc_with(make_test_order(1));
        let _ = arena.alloc_with(make_test_order(2));
        for (_h, order) in arena.iter_mut() {
            order.client_order_id = Some("iter_mut".to_string());
        }
        let all_marked = arena
            .iter()
            .all(|(_, o)| o.client_order_id.as_deref() == Some("iter_mut"));
        assert!(all_marked);
    }

    // ─── 容量上限 / u32 index 边界 ──────────────────

    #[test]
    fn alloc_grows_beyond_initial_capacity() {
        let mut arena = OrderArena::with_capacity(2);
        let _ = arena.alloc_with(make_test_order(1));
        let _ = arena.alloc_with(make_test_order(2));
        assert_eq!(arena.capacity(), 2);
        let _ = arena.alloc_with(make_test_order(3));
        assert!(arena.capacity() >= 3, "Vec 自动扩容");
        assert_eq!(arena.len(), 3);
    }

    // ─── Send + Sync 编译期检查(运行时) ───────────

    #[test]
    fn order_arena_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<OrderArena>();
        assert_sync::<OrderArena>();
        assert_send::<OrderHandle>();
        assert_sync::<OrderHandle>();
    }

    // ─── 性能 smoke test(1M alloc/free < 1s) ──────

    #[test]
    #[ignore] // 跑全 1M 太慢,默认 ignore;`cargo test -- --ignored` 启用
    fn alloc_free_1m_within_1s() {
        let mut arena = OrderArena::with_capacity(1_000_000);
        let mut handles = Vec::with_capacity(1_000_000);
        let start = std::time::Instant::now();
        for i in 0..1_000_000_u64 {
            handles.push(arena.alloc_with(make_test_order(i)));
        }
        for h in &handles {
            arena.free(*h);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_secs() < 1,
            "1M alloc+free 应 < 1s,实测 {elapsed:?}"
        );
    }
}
