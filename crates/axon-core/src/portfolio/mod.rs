//! 投资组合（Portfolio）
//!
//! 跟踪多币种现金、多资产持仓、盈亏、净值等核心状态。
//!
//! TDD 规范：[`axon-design/01-tdd/01-phase1-core/05-portfolio.md`](../../../../axon-design/01-tdd/01-phase1-core/05-portfolio.md)
//!
//! # 模块组织
//!
//! - [`currency`]：货币代码（ISO 4217 三字母）
//! - [`position`]：单资产持仓
//! - [`trade_record`]：round-trip 交易记录(0.6.0 起,只记已平仓)
//! - [`fill_record`]：每笔 fill 完整记录(0.7.0 新增,补 L3 级别可观测性)
//! - [`snapshot`]：投资组合快照（用于时间序列）
//! - [`error`]：错误类型
//! - [`core`]：[`Portfolio`] 主结构

pub mod core;
pub mod currency;
pub mod error;
pub mod fill_record;
pub mod position;
pub mod snapshot;
pub mod trade_record;

pub use core::Portfolio;
pub use currency::Currency;
pub use error::{PortfolioError, PortfolioResult};
pub use fill_record::FillRecord;
pub use position::Position;
pub use snapshot::PortfolioSnapshot;
pub use trade_record::TradeRecord;
