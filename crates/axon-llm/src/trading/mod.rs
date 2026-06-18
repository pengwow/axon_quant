//! 交易工具模块:place_order / query_portfolio 工具与后端抽象
//!
//! 详见 `docs/superpowers/specs/2026-06-16-axon-llm-trading-tools-design.md`。
//!
//! 适配器后端(均为 opt-in feature):
//! - `trading-exchange`:`ExchangeTradingBackend`(已交付,2026-06-17)
//! - `trading-oms`:`OmsTradingBackend`(已交付,2026-06-17)
//! - `trading-backtest`:`BacktestTradingBackend`(2026-06-17)

pub mod backend;
pub mod cancel_order_tool;
pub mod metrics;
pub mod mock;
pub mod place_order_tool;
pub mod query_portfolio_tool;
pub mod replace_order_tool;
pub mod safety;
pub mod types;

#[cfg(feature = "trading-exchange")]
pub mod exchange;

#[cfg(feature = "trading-oms")]
pub mod oms;

#[cfg(feature = "trading-backtest")]
pub mod backtest;

pub use backend::{TradingBackend, TradingError};
pub use cancel_order_tool::CancelOrderTool;
pub use metrics::{
    LabelSet, LabeledCounter, LatencyHistogram, LatencySample, MetricKind, MetricSample, RiskRule,
    TradingMetrics,
};
pub use mock::{FailureInjector, MockTradingBackend};
pub use place_order_tool::PlaceOrderTool;
pub use query_portfolio_tool::QueryPortfolioTool;
pub use replace_order_tool::ReplaceOrderTool;
pub use safety::{AlwaysOpenGate, DailyCounter, PendingOrder, RiskGate, RiskLimits, SafetyMode};
pub use types::{
    BalanceSnapshot, CancelOrderArgs, CurrencyBalance, OrderAck, OrderKind, OrderSide, OrderStatus,
    PlaceOrderArgs, PortfolioSnapshot, PositionSnapshot, QueryPortfolioArgs, ReplaceOrderArgs,
    TimeInForce,
};

#[cfg(feature = "trading-exchange")]
pub use exchange::{ExchangeTradingBackend, SymbolMap};

#[cfg(feature = "trading-oms")]
pub use oms::OmsTradingBackend;

#[cfg(feature = "trading-backtest")]
pub use backtest::BacktestTradingBackend;
