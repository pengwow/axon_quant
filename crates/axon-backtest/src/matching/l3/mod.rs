//! L3 撮合引擎：多资产路由 / 暗池 / 批量拍卖
//!
//! TDD 规范：[`axon-design/01-tdd/01-phase1-core/11-matching-l3.md`](../../../../axon-design/01-tdd/01-phase1-core/11-matching-l3.md)
//!
//! # 模块组织
//!
//! - [`types`]：Venue / CrossPair / BatchMode / DarkOrder / AuctionResult / L3Stats / L2Snapshot
//! - [`engine_l3`]：MultiAssetMatchingEngine 多资产路由核心
//! - [`book`]：L3Book 完整 L3 可见视图(Phase 3.2 新增)
//! - [`dark_pool`]：暗池撮合
//! - [`auction`]：批量拍卖清算价格
//! - [`error`]：MatchingL3Error

pub mod auction;
pub mod book;
pub mod dark_pool;
pub mod engine_l3;
pub mod error;
pub mod types;

pub use auction::{AuctionResult, BatchMode, find_clearing_price};
pub use book::{L3Book, L3Order};
pub use dark_pool::{DarkOrder, try_dark_match};
pub use engine_l3::{ArbitrageOpportunity, L3Stats, MultiAssetMatchingEngine};
pub use error::{MatchingL3Error, MatchingL3Result};
pub use types::{CrossPair, L2Snapshot, MatchingEngineSnapshot, PriceLevel, Venue};
