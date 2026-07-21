//! 部分成交追踪器 (0.8.0 Phase 3.2 A1.1)
//!
//! # 动机
//!
//! L1 撮合引擎在 `L1Book::match_against_asks` / `match_against_bids` 中
//! 已经能产生 `MatchFill` 列表(瞬时撮合记录),
//! 但**没有持久化按 `OrderId` 索引的 fill 链**:
//!
//! - 同一笔订单多次部分成交,fill 列表与 order 状态(PartiallyFilled)耦合,
//!   策略层 / 对账层无法从外部查询"该 order 历次 fill 的完整时间线"
//! - 订单部分成交后取消,fill 列表已"飘散"(无关联 order 状态信息),
//!   无法判断"该 cancel 是不是发生在部分成交后"
//! - 撮合后的 fill 链(per-order)与 MatchFill(per-match)是同一份数据的两
//!   个视图,目前 L1 缺前者
//!
//! `PartialFillTracker` 解决上述三个问题:
//! - 按 `OrderId` 索引的 `Vec<FillRecord>` 链(`HashMap<OrderId, Vec<FillRecord>>`)
//! - 提供 `mark_filled` / `mark_cancelled_after_partial` 在订单生命周期
//!   关键时刻升级该链最后一条 fill 的状态
//! - 不持有 `Order` 引用,fill 链与 order 状态独立可观察
//!
//! # 设计要点
//!
//! - **跨 instrument 共享**:tracker 挂在 [`L1MatchingEngine`](super::engine::L1MatchingEngine)
//!   上(`tracker: PartialFillTracker` 字段),与 `trade_sequence` 一样,跨 book
//!   共享(全局 fill 序号 + 全局 fill 链)
//! - **fill 链与 order 状态独立**:`PartialFillTracker` 不读 `Order.status`,
//!   状态机由调用方(L1Book 撮合循环 + L1MatchingEngine::submit/cancel)显式驱动
//! - **per-instrument 清空**:`clear_for_instrument` 只清掉属于该 instrument
//!   的 fill 链(per-leg seed 用,与 `clear_book_for` 同步)
//!
//! # 不属于这里
//!
//! - **fill 业务语义**:`MatchFill`(撮合期瞬时)与 `FillRecord`(撮合后持久化
//!   追踪)1:1 对应,但语义不同。`FillRecord.state` 是后加的状态机。
//! - **事件回调**:`PartialFillTracker` 不广播状态变化,Phase 4 Strategy trait
//!   落地时再加 event bus。
//!
//! # 状态机时序图
//!
//! ```text
//! order 入簿
//!   ↓
//! [撮合] 每次 fill 产生 → track_fill → state = Active
//!   ↓
//! ┌─────────────┬──────────────────────┐
//! │ 继续撮合     │ 全部成交(is_filled)  │
//! │ 推入新 fill  │ mark_filled          │
//! │ 保持 Active  │ 最后 fill → Filled   │
//! └─────────────┴──────────────────────┘
//!                 ↓
//!          [cancel] 仍有 remaining?
//!           ├─ 有 → mark_cancelled_after_partial → 最后 fill 升级
//!           └─ 无 → 已 Filled,不再处理
//! ```

use std::collections::HashMap;

use axon_core::order::{FillRecord, FillState, OrderId};
use axon_core::types::Instrument;

use super::types::MatchFill;

/// 部分成交追踪器
///
/// # 字段布局
///
/// - `chain`:`HashMap<OrderId, Vec<FillRecord>>`,按订单 id 索引
///   fill 链,Vec 按 fill_id 升序(撮合循环单线程 push 即可保证)
/// - `order_instrument`:`HashMap<OrderId, Instrument>`,记录每个
///   order(无论 taker 还是 maker)所属 instrument,供 `clear_for_instrument`
///   过滤用。
///
/// # 线程安全
///
/// 当前 L1MatchingEngine 是单线程(BacktestEngine 串行调用),
/// 暂未加 `parking_lot::Mutex`。`HashMap` 操作全部是 `&mut self`
/// 单线程,无 race。后续如果 BacktestEngine 并行化(0.9.0 Stage 3),
/// tracker 需要包 `Mutex`(或拆成 `DashMap`)。
#[derive(Debug, Default)]
pub struct PartialFillTracker {
    /// per-order fill 链:`order_id -> [fill_1, fill_2, ...]`
    chain: HashMap<OrderId, Vec<FillRecord>>,
    /// per-order instrument 标记(taker + maker 都记),供 `clear_for_instrument` 过滤
    ///
    /// 在 [`Self::track_fill`] 中同步记录:taker_order_id 和
    /// maker_order_id 都登记到该 hashmap,确保 `clear_for_instrument`
    /// 能正确清空整个 instrument 的所有 fill 链(包括纯 maker 链)。
    order_instrument: HashMap<OrderId, Instrument>,
}

impl PartialFillTracker {
    /// 创建空 tracker
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次撮合产生的 fill(同时 push 到 taker 链 + maker 链)
    ///
    /// # 参数
    ///
    /// - `fill`:撮合循环产出的 `MatchFill`(瞬时记录)
    /// - `instrument`:本次撮合的 instrument(taker 和 maker 都属于该 instrument,
    ///   因为 L1Book 是 per-instrument 路由,`match_against_*` 接收的
    ///   `taker_instrument` 就是 book 所属 instrument)
    ///
    /// # 副作用
    ///
    /// - taker 链 push 一条 `FillRecord { state: Active }`
    /// - maker 链 push 一条 `FillRecord { state: Active }`
    /// - `order_instrument[taker_id] = instrument`(覆盖式写入,同 order
    ///   多次 fill 写同一个 instrument,no-op 效果)
    /// - `order_instrument[maker_id] = instrument`(同上)
    pub fn track_fill(&mut self, fill: &MatchFill, instrument: &Instrument) {
        let record = FillRecord::new(
            fill.fill_id,
            fill.taker_order_id,
            fill.maker_order_id,
            fill.price,
            fill.quantity,
            fill.timestamp,
        );
        // taker 链
        self.chain
            .entry(fill.taker_order_id)
            .or_default()
            .push(record.clone());
        // maker 链
        self.chain
            .entry(fill.maker_order_id)
            .or_default()
            .push(record);
        // 记录 taker + maker 所属 instrument(供 per-instrument 清空用)
        self.order_instrument
            .insert(fill.taker_order_id, instrument.clone());
        self.order_instrument
            .insert(fill.maker_order_id, instrument.clone());
    }

    /// 标记某 order 全部成交 — 升级其最后一条 fill 为 `Filled`
    ///
    /// # 调用时机
    ///
    /// - L1MatchingEngine::submit 中,撮合循环结束后 `taker.is_filled()`
    /// - L1Book::match_against_* 中,撮合循环内 `maker.is_filled()`(maker
    ///   正在被吃光时立即升级,而不是等下个 fill)
    ///
    /// # 行为
    ///
    /// - 该 order 的最后一条 fill 状态从 `Active` 升级为 `Filled`
    /// - 若该 order 无 fill 链(no-op,L1 不会发生但防御)
    /// - 若最后 fill 已是 `Filled` / `Cancelled*` 终态(no-op,非法转换被静默吞)
    ///
    /// # 静默吞错
    ///
    /// `FillState::transition_to` 返回 `Result`,我们用 `let _` 吞掉 Err。
    /// 这是 invariant violation,理论上不应发生:
    ///
    /// - 全部成交的 order 不应再被 cancel(状态机拒)
    /// - 已被 cancel 的 order 不应再 mark_filled
    ///
    /// 若发生,可能源于外部调用方手动改 `Order.status`,会留 log 后续排查。
    /// 0.8.0 决策:不 panic,允许 fill 链自我恢复(留 active 状态)。后续
    /// Phase 4 加 event bus 时此处会发出告警。
    pub fn mark_filled(&mut self, order_id: OrderId) {
        if let Some(chain) = self.chain.get_mut(&order_id)
            && let Some(last) = chain.last_mut()
            && let Ok(new_state) = last.state.transition_to(FillState::Filled)
        {
            last.state = new_state;
        }
    }

    /// 标记某 order 部分成交后被取消 — 升级其最后一条 fill 为 `CancelledAfterPartial`
    ///
    /// # 调用时机
    ///
    /// L1MatchingEngine::cancel 中,从 book 移除 order 后:
    ///
    /// - 若该 order 在 fill 链里**有 fill**(被部分成交过)→ mark_cancelled_after_partial
    /// - 若 fill 链**为空**(从未成交)→ 啥都不做(无 fill 可升级,无 CancelledNoFill 概念)
    ///
    /// # 行为
    ///
    /// - 该 order 的最后一条 fill 状态从 `Active` 升级为 `CancelledAfterPartial`
    /// - 若该 order 无 fill 链(no-op)
    /// - 若最后 fill 已是 `Filled`(理论不会,因为 Filled 状态不可被 cancel)
    ///   或 `CancelledAfterPartial`(重复 cancel)→ no-op
    pub fn mark_cancelled_after_partial(&mut self, order_id: OrderId) {
        if let Some(chain) = self.chain.get_mut(&order_id)
            && let Some(last) = chain.last_mut()
            && let Ok(new_state) = last.state.transition_to(FillState::CancelledAfterPartial)
        {
            last.state = new_state;
        }
    }

    /// 读取指定 order 的 fill 链(只读)
    ///
    /// 返回 `None` 表示该 order 从未成交(链不存在)。**不创建空链**。
    #[inline]
    pub fn chain(&self, order_id: OrderId) -> Option<&[FillRecord]> {
        self.chain.get(&order_id).map(Vec::as_slice)
    }

    /// 指定 order 的 fill 链长度
    ///
    /// `0` 表示该 order 从未成交。等价于 `chain(order_id).map_or(0, |c| c.len())`。
    #[inline]
    pub fn chain_len(&self, order_id: OrderId) -> usize {
        self.chain.get(&order_id).map_or(0, Vec::len)
    }

    /// 全局 fill 总数(所有 order 的 fill 链长度之和)
    ///
    /// 注:每笔撮合产生 **2 条** record(taker 链 + maker 链),所以这个数字
    /// 是 `MatchFill` 总数的 **2 倍**。
    #[inline]
    pub fn total_records(&self) -> usize {
        self.chain.values().map(Vec::len).sum()
    }

    /// 全局被追踪的 order 数
    #[inline]
    pub fn tracked_orders(&self) -> usize {
        self.chain.len()
    }

    /// 清空全部 fill 链 + instrument 索引
    ///
    /// 用途:与 `L1MatchingEngine::clear_book` 同步,跨 instrument 一次性清空。
    pub fn clear(&mut self) {
        self.chain.clear();
        self.order_instrument.clear();
    }

    /// 清空指定 instrument 的 fill 链(per-leg seed 用)
    ///
    /// 用途:与 `L1MatchingEngine::clear_book_for` 同步,只清掉属于该
    /// instrument 的 fill 链(不破坏其他 instrument 的对账数据)。
    ///
    /// # 实现
    ///
    /// 遍历 `order_instrument`,过滤出 instrument 匹配的 order_id,批量
    /// 从 `chain` 中 `remove`。O(N) where N = 已被追踪的 order 总数。
    ///
    /// # 覆盖范围
    ///
    /// 既清 taker 链,也清 maker 链(因为 `order_instrument` 在
    /// [`Self::track_fill`] 时同时记录 taker 和 maker)。
    pub fn clear_for_instrument(&mut self, instrument: &Instrument) {
        // 收集要保留的 order_id(非该 instrument)
        let to_remove: Vec<OrderId> = self
            .order_instrument
            .iter()
            .filter_map(|(id, inst)| (inst == instrument).then_some(*id))
            .collect();
        for id in to_remove {
            self.chain.remove(&id);
            self.order_instrument.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matching::types::MatchFill;
    use axon_core::order::FillState;
    use axon_core::time::Timestamp;
    use axon_core::types::{Price, Quantity, SpotInstrument, Symbol};

    fn btc() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    fn make_match_fill(fill_id: u64, taker: u64, maker: u64, qty: f64) -> MatchFill {
        MatchFill {
            fill_id,
            taker_order_id: taker,
            maker_order_id: maker,
            price: Price::from_f64(100.0),
            quantity: Quantity::from_f64(qty),
            taker_side: axon_core::market::Side::Buy,
            timestamp: Timestamp::from_nanos(1_000 * fill_id as i64),
        }
    }

    // ── 基础 API ──────────────────────────────────────────────

    #[test]
    fn test_new_is_empty() {
        let tracker = PartialFillTracker::new();
        assert_eq!(tracker.total_records(), 0);
        assert_eq!(tracker.tracked_orders(), 0);
    }

    #[test]
    fn test_track_fill_pushes_to_both_chains() {
        let mut tracker = PartialFillTracker::new();
        let fill = make_match_fill(1, 100, 200, 1.0);
        tracker.track_fill(&fill, &btc());

        // taker 链有 1 条
        assert_eq!(tracker.chain_len(100), 1);
        // maker 链有 1 条
        assert_eq!(tracker.chain_len(200), 1);
        // 总 record = 2(taker + maker)
        assert_eq!(tracker.total_records(), 2);
        assert_eq!(tracker.tracked_orders(), 2);
    }

    #[test]
    fn test_track_fill_appends_to_existing_chain() {
        let mut tracker = PartialFillTracker::new();
        // 同 order 多次 fill
        tracker.track_fill(&make_match_fill(1, 100, 200, 1.0), &btc());
        tracker.track_fill(&make_match_fill(2, 100, 300, 1.0), &btc());
        tracker.track_fill(&make_match_fill(3, 100, 400, 1.0), &btc());

        assert_eq!(tracker.chain_len(100), 3);
        // 链按 fill_id 升序(撮合循环 push 顺序)
        let chain = tracker.chain(100).unwrap();
        assert_eq!(chain[0].fill_id, 1);
        assert_eq!(chain[1].fill_id, 2);
        assert_eq!(chain[2].fill_id, 3);
    }

    #[test]
    fn test_chain_returns_none_for_unknown_order() {
        let tracker = PartialFillTracker::new();
        assert!(tracker.chain(999).is_none());
        assert_eq!(tracker.chain_len(999), 0);
    }

    #[test]
    fn test_track_fill_initial_state_is_active() {
        let mut tracker = PartialFillTracker::new();
        tracker.track_fill(&make_match_fill(1, 100, 200, 1.0), &btc());
        let chain = tracker.chain(100).unwrap();
        assert_eq!(chain[0].state, FillState::Active);
        assert!(!chain[0].is_terminal());
    }

    // ── 状态机升级 ────────────────────────────────────────────

    #[test]
    fn test_mark_filled_upgrades_last_record() {
        let mut tracker = PartialFillTracker::new();
        tracker.track_fill(&make_match_fill(1, 100, 200, 1.0), &btc());
        tracker.track_fill(&make_match_fill(2, 100, 300, 1.0), &btc());

        tracker.mark_filled(100);

        let chain = tracker.chain(100).unwrap();
        // 最后一条升级
        assert_eq!(chain[1].state, FillState::Filled);
        // 前序保持 Active
        assert_eq!(chain[0].state, FillState::Active);
    }

    #[test]
    fn test_mark_filled_on_empty_chain_is_noop() {
        let mut tracker = PartialFillTracker::new();
        // 没 fill 链,mark_filled 应 no-op,不能 panic
        tracker.mark_filled(999);
        assert_eq!(tracker.total_records(), 0);
    }

    #[test]
    fn test_mark_cancelled_after_partial_upgrades_last_record() {
        let mut tracker = PartialFillTracker::new();
        tracker.track_fill(&make_match_fill(1, 100, 200, 1.0), &btc());
        tracker.track_fill(&make_match_fill(2, 100, 300, 1.0), &btc());

        tracker.mark_cancelled_after_partial(100);

        let chain = tracker.chain(100).unwrap();
        assert_eq!(chain[1].state, FillState::CancelledAfterPartial);
        // 前序仍 Active
        assert_eq!(chain[0].state, FillState::Active);
    }

    #[test]
    fn test_mark_cancelled_after_partial_on_empty_chain_is_noop() {
        let mut tracker = PartialFillTracker::new();
        // 无 fill 链 → no-op(无 fill 可升级)
        tracker.mark_cancelled_after_partial(999);
        assert_eq!(tracker.total_records(), 0);
    }

    #[test]
    fn test_mark_filled_after_cancelled_is_noop() {
        // 状态机:Filled / Cancelled* 不可互转
        // 已 CancelledAfterPartial 的 fill,再 mark_filled 应 no-op
        let mut tracker = PartialFillTracker::new();
        tracker.track_fill(&make_match_fill(1, 100, 200, 1.0), &btc());
        tracker.mark_cancelled_after_partial(100);
        tracker.mark_filled(100);

        let chain = tracker.chain(100).unwrap();
        // 状态保持 CancelledAfterPartial(不被覆盖)
        assert_eq!(chain[0].state, FillState::CancelledAfterPartial);
    }

    // ── 清空 ────────────────────────────────────────────────

    #[test]
    fn test_clear_resets_everything() {
        let mut tracker = PartialFillTracker::new();
        tracker.track_fill(&make_match_fill(1, 100, 200, 1.0), &btc());
        tracker.track_fill(&make_match_fill(2, 300, 400, 1.0), &btc());
        assert_eq!(tracker.tracked_orders(), 4);

        tracker.clear();
        assert_eq!(tracker.total_records(), 0);
        assert_eq!(tracker.tracked_orders(), 0);
        assert!(tracker.chain(100).is_none());
    }

    #[test]
    fn test_clear_for_instrument_filters_by_taker() {
        let mut tracker = PartialFillTracker::new();
        let eth = Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        });

        // btc fill:taker 100 / maker 200
        tracker.track_fill(&make_match_fill(1, 100, 200, 1.0), &btc());
        // eth fill:taker 300 / maker 400
        tracker.track_fill(&make_match_fill(2, 300, 400, 1.0), &eth);

        // 清 btc:taker 100 的链应消失,但 taker_instrument 里的 100 也消失
        // maker 200 在 btc 链里**也消失**(taker 100 决定 taker_instrument,
        // 而 chain 是按 taker 索引的,这里有问题)
        // 重新审视:chain[taker_100] 是一条 record,清掉 taker_100 即可
        tracker.clear_for_instrument(&btc());

        // btc fill 消失
        assert!(tracker.chain(100).is_none(), "btc taker 链应消失");
        // eth fill 仍在
        assert!(tracker.chain(300).is_some(), "eth taker 链应保留");
        assert_eq!(tracker.chain(300).unwrap().len(), 1);
    }

    #[test]
    fn test_clear_for_instrument_no_match_is_noop() {
        let mut tracker = PartialFillTracker::new();
        tracker.track_fill(&make_match_fill(1, 100, 200, 1.0), &btc());

        let eth = Instrument::Spot(SpotInstrument {
            base: Symbol::from("ETH"),
            quote: Symbol::from("USDT"),
        });
        tracker.clear_for_instrument(&eth);

        // 没匹配,btc 链仍存在
        assert!(tracker.chain(100).is_some());
    }

    // ── 端到端场景 ────────────────────────────────────────────

    #[test]
    fn test_e2e_full_fill_lifecycle() {
        // 场景:taker 100 多次部分成交后全成
        // fill 1: taker 100 vs maker 200, qty 0.3 → taker 仍 PartiallyFilled
        // fill 2: taker 100 vs maker 300, qty 0.3 → taker 仍 PartiallyFilled
        // fill 3: taker 100 vs maker 400, qty 0.4 → taker 全成 → mark_filled
        let mut tracker = PartialFillTracker::new();
        tracker.track_fill(&make_match_fill(1, 100, 200, 0.3), &btc());
        tracker.track_fill(&make_match_fill(2, 100, 300, 0.3), &btc());
        tracker.track_fill(&make_match_fill(3, 100, 400, 0.4), &btc());

        tracker.mark_filled(100);

        let chain = tracker.chain(100).unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].state, FillState::Active);
        assert_eq!(chain[1].state, FillState::Active);
        assert_eq!(chain[2].state, FillState::Filled);

        // maker 各自一条 fill(状态仍 Active,因为 maker 还在簿)
        assert_eq!(tracker.chain_len(200), 1);
        assert_eq!(tracker.chain(200).unwrap()[0].state, FillState::Active);
        assert_eq!(tracker.chain_len(300), 1);
        assert_eq!(tracker.chain_len(400), 1);
    }

    #[test]
    fn test_e2e_partial_then_cancelled_lifecycle() {
        // 场景:taker 100 部分成交(0.3)后被 cancel
        let mut tracker = PartialFillTracker::new();
        tracker.track_fill(&make_match_fill(1, 100, 200, 0.3), &btc());

        tracker.mark_cancelled_after_partial(100);

        let chain = tracker.chain(100).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].state, FillState::CancelledAfterPartial);
    }

    #[test]
    fn test_e2e_maker_completely_filled() {
        // 场景:maker 200 部分成交后,在第二次 fill 中被完全吃光
        // fill 1: taker 100 vs maker 200, qty 0.3 → maker 仍 0.7 remaining
        // fill 2: taker 300 vs maker 200, qty 0.7 → maker 全成 → mark_filled(maker 200)
        let mut tracker = PartialFillTracker::new();
        tracker.track_fill(&make_match_fill(1, 100, 200, 0.3), &btc());
        tracker.track_fill(&make_match_fill(2, 300, 200, 0.7), &btc());

        tracker.mark_filled(200); // maker 全成

        // maker 200 链:2 条,最后一条 Filled
        let chain = tracker.chain(200).unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].state, FillState::Active);
        assert_eq!(chain[1].state, FillState::Filled);
    }
}
