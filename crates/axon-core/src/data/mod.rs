//! 历史市场数据源抽象(0.8.0 Phase 2 B1 新增)
//!
//! 为 `PortfolioRiskEngine` 计算 gamma / vega 提供 mark 历史与 IV。
//! 0.7.0 现状:gamma / vega 全 0,因 `push_mark` 只存最新 1 帧、无 IV 源。
//!
//! # 模块组织
//!
//! - [`source`]:核心 trait [`MarketDataSource`]
//! - [`inmemory`]:进程内增量实现 [`InMemoryMarketData`]
//! - [`csv`]:CSV 文件加载实现 [`CsvMarketData`]
//!
//! 真实源(Deribit / Akash / Binance options)推迟到 0.9.0。

pub mod csv;
pub mod inmemory;
pub mod source;

pub use csv::{CsvMarketData, CsvMarketDataError};
pub use inmemory::InMemoryMarketData;
pub use source::{MarkPoint, MarketDataSource};
