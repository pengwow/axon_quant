//! 流式回测引擎
//!
//! 支持实时行情接入、模拟盘运行的流式回测引擎。
//! 在现有批处理回测引擎基础上，添加流式事件处理能力。
//!
//! ## 核心功能
//!
//! - **统一数据源接口**：支持交易所 WebSocket 和文件回放
//! - **流式事件处理**：实时处理市场数据事件
//! - **模拟盘模式**：注入延迟和滑点的模拟交易
//! - **实时指标采集**：集成 axon-monitor 监控
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! use axon_backtest::streaming::{StreamingEngine, TradingMode};
//!
//! let mut engine = StreamingEngine::new(TradingMode::PaperTrading);
//! // engine.subscribe(data_source).await;
//! // engine.on_market_event(event);
//! ```
//!
//! # 已废弃
//!
//! 本模块由 `axon-kernel` 的 `KernelPump` + `PaperSink` 取代（同一份历史数据，
//! 内核主回路在回测 / paper / live 下跑同一条路径，模拟成交入口只剩 `PaperSink`）。
//! M6 之后的新代码不得使用本模块；模块将在下个大版本直接移除，现有类型
//! （[`MarketDataEvent`] / [`StreamingEngine`] / [`PaperTradingEngine`] 等）只保留
//! 到那时为止。
//!
//! 这里只做文档标注、不加 `#[deprecated]` 属性：工作区内仍有 legacy 测试
//! （`tests/e2e_streaming_paper.rs`、`tests/streaming_metrics_e2e.rs` 等）在跑这条
//! 路径，属性会把工作区门禁的 `-D warnings` 打成失败。

mod data_source;
mod engine;
pub mod l3_diff;
mod metrics;
mod paper_trading;
mod report;
mod strategy;

pub use data_source::{
    CsvMapping, ExchangeStreamSource, MarketDataEvent, ReplayStreamSource, StreamDataSource,
    StreamError, TimestampUnit,
};
pub use engine::{EngineSnapshot, StreamingEngine, StreamingSnapshot, TradingMode};
pub use l3_diff::{L3BookDiff, L3BookSubscriber, SubscriberKind};
pub use metrics::{EquityPoint, StreamingMetrics, StreamingMetricsSnapshot};
pub use paper_trading::{PaperTradingEngine, SimulatedExchange};
pub use report::{ReportError, ReportFormat, StreamingReport};
pub use strategy::{SmaCrossover, StrategyAction, StreamingStrategy};
