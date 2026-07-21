//! per-fill 元数据 + 状态机(0.8.0 Phase 3.2 A1.1)
//!
//! # 动机
//!
//! 撮合引擎原本只对外暴露 `MatchFill`(`axon-backtest` 侧的 `MatchFill`),
//! 但缺少 **per-order fill 链式追踪**:
//!
//! - 同一笔订单多次部分成交时,只能从 `Order.filled_quantity` 看累计,
//!   看不到"哪几次成交、对手方分别是谁、什么时候成交的"
//! - 订单被部分成交后取消,没有"最后那次 fill 的状态"语义(普通 fill
//!   vs cancel-after-partial)
//! - 策略层 / 对账层需要一个稳定可序列化的 fill 链结构(便于对账 / 重放)
//!
//! `FillRecord` + `FillState` 解决上述三个问题,挂在 `PartialFillTracker`
//! (`axon-backtest/src/matching/tracker.rs`)上,按 `OrderId` 索引。
//!
//! # 设计要点
//!
//! - **`Order` 不变**:`FillRecord` 是独立类型,不污染 `Order` 字段,保持
//!   `Order` 的 serde schema 稳定(0.7.x 序列化数据兼容)。
//! - **fill 链存储位置**:`PartialFillTracker`(`HashMap<OrderId, Vec<FillRecord>>`),
//!   挂在 L1Book 内部;对外通过 `engine.fill_tracker().chain(order_id)` 查。
//! - **状态机**:`FillState` 4 态,记录"该次 fill 在订单生命周期中的最终态":
//!   - `Active` — 撮合时立即记入,order 仍在簿(可能后续被 cancel 升级状态)
//!   - `Filled` — 这是该 order 的最后一次 fill 且 order 已全部成交
//!   - `CancelledAfterPartial` — 之前有 fill,被 cancel 时仍有 remaining
//!   - `CancelledNoFill` — 撮合时无 fill(理论上不会出现;保留供对账)
//! - **生命周期**:由 L1Book 在撮合 / cancel 时机显式调用 `PartialFillTracker`
//!   方法更新;不依赖 `Order.status` 反推(fill 链与 order 状态独立可观察)。
//!
//! # 不属于这里的
//!
//! - **撮合语义**:`MatchFill`(`axon-backtest`)仍是撮合期的瞬时记录,`FillRecord`
//!   是 post-match 持久化追踪,两者 1:1 对应但语义不同。
//! - **事件回调钩子**:`FillState` 状态转换目前无自动广播,Phase 4 Strategy trait
//!   落地时再加 event bus。当前策略层通过 `SubmitResult.fills` 自取。

use serde::{Deserialize, Serialize};

use crate::order::OrderId;
use crate::time::Timestamp;
use crate::types::{Price, Quantity};

/// 单次成交的元数据 + 状态
///
/// # 字段映射
///
/// 与 `MatchFill`(`axon-backtest` 侧)1:1 对应,但额外记录:
/// - `state`:`FillState` 状态机(在 cancel / 全部成交 时机更新)
/// - `taker_side`:`Side` 的序列化版本(MatchFill 已有,不重复)
///
/// # 序列化
///
/// - `default` skip:`state` 默认为 `Active`,反序列化缺省时退化为 `Active`
///   (向前兼容 0.8.0 之前数据)
/// - `quantity` / `price` 用 axon-core 自身的 newtype
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillRecord {
    /// 全局唯一 fill id(由 `L1Book.trade_sequence: AtomicU64` 分配)
    pub fill_id: u64,
    /// taker(主动吃单方)订单 id
    pub taker_order_id: OrderId,
    /// maker(挂单方)订单 id
    pub maker_order_id: OrderId,
    /// 成交价格
    pub price: Price,
    /// 成交数量
    pub quantity: Quantity,
    /// 成交时间戳
    pub timestamp: Timestamp,
    /// fill 状态机
    ///
    /// `Active` 是默认值,反序列化时缺省 → `Active`(向前兼容 0.8.0 之前数据)。
    #[serde(default)]
    pub state: FillState,
}

impl FillRecord {
    /// 构造新 fill 记录,默认状态 `Active`
    ///
    /// `taker_side` 由撮合层从 taker Order 读出后传入,这里不持有 `Order`
    /// 引用(避免 borrow 冲突 + 减小克隆成本)。
    pub fn new(
        fill_id: u64,
        taker_order_id: OrderId,
        maker_order_id: OrderId,
        price: Price,
        quantity: Quantity,
        timestamp: Timestamp,
    ) -> Self {
        Self {
            fill_id,
            taker_order_id,
            maker_order_id,
            price,
            quantity,
            timestamp,
            state: FillState::Active,
        }
    }

    /// 是否处于终态(Filled / Cancelled*)
    #[inline]
    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }
}

/// fill 状态机
///
/// 描述"该次 fill 在所属订单生命周期中的最终态":
///
/// - 撮合时立即记入 [`FillRecord`],`state = Active`
/// - 同一笔 order 后续 fill 继续 push,前序 fill 状态不自动变
/// - 当该 order **全部成交** 时,最后一条 fill 状态升级为 `Filled`,
///   前序 fill 状态保持 `Active`(因它们当时确实是"中间态")
/// - 当该 order **被 cancel 时仍有 remaining**(部分成交后取消),最后一条
///   fill 状态升级为 `CancelledAfterPartial`
/// - 撮合时 fill 数量为 0(理论上 L1Book 已防御)记 `CancelledNoFill`,
///   仅供对账使用,正常流程不应出现
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum FillState {
    /// 撮合时记录,order 仍在簿(可能后续升级到 Filled / CancelledAfterPartial)
    #[default]
    Active,
    /// 全部成交 — 该 fill 是 order 的最后一次 fill 且使 order 状态进 Filled
    Filled,
    /// 部分成交后取消 — 该 fill 是 order 的最后 fill,order 被 cancel 时仍有 remaining
    CancelledAfterPartial,
    /// 撮合时无成交(防御性,正常不应出现;仅做对账/告警)
    CancelledNoFill,
}

impl FillState {
    /// 是否处于终态(`Filled` / `CancelledAfterPartial` / `CancelledNoFill`)
    ///
    /// `Active` 是中间态(撮合后到 order 生命周期结束前),其余 3 个是终态。
    #[inline]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Active)
    }

    /// 状态机是否允许 Active → X 的转换
    ///
    /// 合法转换:`Active → Filled` / `Active → CancelledAfterPartial` /
    /// `Active → CancelledNoFill`(后者用于 qty=0 防御)
    ///
    /// 非法:任何终态之间的转换(Filled → CancelledAfterPartial 等),
    /// 一旦 fill 进入终态,该 fill 不再变化。
    #[inline]
    pub fn can_transition_to(self, target: FillState) -> bool {
        match (self, target) {
            (Self::Active, Self::Filled) => true,
            (Self::Active, Self::CancelledAfterPartial) => true,
            (Self::Active, Self::CancelledNoFill) => true,
            // 终态不可再变(包括 self == target 的 no-op,显式拒绝避免误用)
            _ => false,
        }
    }

    /// 升级到目标状态,非法转换返回 `Err`
    ///
    /// 防御性 API,正常流程由 `PartialFillTracker`(`axon-backtest` 侧)
    /// 内部调用,不会触发 Err。
    pub fn transition_to(self, target: FillState) -> Result<FillState, FillState> {
        if self.can_transition_to(target) {
            Ok(target)
        } else {
            Err(self)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record() -> FillRecord {
        FillRecord::new(
            1,
            100,
            200,
            Price::from_f64(100.0),
            Quantity::from_f64(1.0),
            Timestamp::from_nanos(1_000),
        )
    }

    #[test]
    fn test_fill_record_new_is_active() {
        let r = make_record();
        assert_eq!(r.state, FillState::Active);
        assert!(!r.is_terminal());
    }

    #[test]
    fn test_fill_state_active_is_not_terminal() {
        assert!(!FillState::Active.is_terminal());
    }

    #[test]
    fn test_fill_state_filled_is_terminal() {
        assert!(FillState::Filled.is_terminal());
    }

    #[test]
    fn test_fill_state_cancelled_is_terminal() {
        assert!(FillState::CancelledAfterPartial.is_terminal());
        assert!(FillState::CancelledNoFill.is_terminal());
    }

    #[test]
    fn test_fill_state_transition_active_to_filled_ok() {
        let s = FillState::Active.transition_to(FillState::Filled).unwrap();
        assert_eq!(s, FillState::Filled);
    }

    #[test]
    fn test_fill_state_transition_active_to_cancelled_ok() {
        let s = FillState::Active
            .transition_to(FillState::CancelledAfterPartial)
            .unwrap();
        assert_eq!(s, FillState::CancelledAfterPartial);

        let s2 = FillState::Active
            .transition_to(FillState::CancelledNoFill)
            .unwrap();
        assert_eq!(s2, FillState::CancelledNoFill);
    }

    #[test]
    fn test_fill_state_transition_terminal_to_terminal_errors() {
        // Filled 不可再变
        let result = FillState::Filled.transition_to(FillState::CancelledAfterPartial);
        assert!(result.is_err());

        // Cancelled 不可再变
        let result = FillState::CancelledAfterPartial.transition_to(FillState::Filled);
        assert!(result.is_err());
    }

    #[test]
    fn test_fill_record_serde_roundtrip() {
        let r = make_record();
        let json = serde_json::to_string(&r).unwrap();
        let r2: FillRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn test_fill_record_serde_default_state_when_missing() {
        // 模拟 0.8.0 之前的数据(无 state 字段)→ 反序列化时默认 Active
        let json = r#"{
            "fill_id": 1,
            "taker_order_id": 100,
            "maker_order_id": 200,
            "price": 100.0,
            "quantity": 1.0,
            "timestamp": 1000
        }"#;
        let r: FillRecord = serde_json::from_str(json).unwrap();
        assert_eq!(r.state, FillState::Active);
    }
}
