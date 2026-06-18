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

mod data_source;
mod engine;
mod metrics;
mod paper_trading;

pub use data_source::{ExchangeStreamSource, ReplayStreamSource, StreamDataSource, StreamError};
pub use engine::{EngineSnapshot, StreamingEngine, TradingMode};
pub use paper_trading::{PaperTradingEngine, SimulatedExchange};
