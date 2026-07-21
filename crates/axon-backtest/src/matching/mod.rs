//! L1 撮合引擎
//!
//! 实现价格-时间优先撮合，支持限价单、市价单、IOC、FOK 订单类型。
//!
//! TDD 规范：[`axon-design/01-tdd/01-phase1-core/09-matching-l1.md`](../../../../axon-design/01-tdd/01-phase1-core/09-matching-l1.md)
//!
//! # 模块组织
//!
//! - [`engine`]：L1MatchingEngine 实现 + MatchingEngine trait
//! - [`l2`]：L2MatchingEngine（L1 增强：修改/统计/O(1) 取消/订单簿导入导出）
//! - [`l3`]：MultiAssetMatchingEngine（多资产路由 / 暗池 / 批量拍卖 / 套利）
//! - [`tracker`]：PartialFillTracker(0.8.0 Phase 3.2 A1.1,per-fill 元数据 + 状态机)
//! - [`types`]：撮合相关类型（MatchFill / TradeRole / OrderBookLevel / SubmitResult）
//! - [`error`]：MatchingError

pub mod engine;
pub mod error;
pub mod l2;
pub mod l3;
pub mod router;
pub mod tracker;
pub mod types;

pub use engine::{L1MatchingEngine, MatchingEngine, OrderBookSide, PriceLevel};
pub use error::{MatchingError, MatchingResult};
pub use l2::{
    L2MatchingEngine, MatchingStats, OrderAmend, OrderBookEntry, OrderLocation, build_limit_order,
};
pub use l3::{
    ArbitrageOpportunity, AuctionResult, BatchMode, CrossPair, DarkOrder, L2Snapshot, L3Stats,
    MatchingEngineSnapshot, MatchingL3Error, MatchingL3Result, MultiAssetMatchingEngine,
    PriceLevel as L3PriceLevel, Venue, find_clearing_price,
};
pub use router::{EngineRouter, RoutedEngine, RoutingStrategy};
pub use tracker::PartialFillTracker;
pub use types::{MatchFill, OrderBookLevel, SubmitResult, TradeRole};
