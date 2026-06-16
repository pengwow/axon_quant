//! 交易工具模块:place_order / query_portfolio 工具与后端抽象
//!
//! 详见 `docs/superpowers/specs/2026-06-16-axon-llm-trading-tools-design.md`。

pub mod backend;
pub mod mock;
pub mod place_order_tool;
pub mod query_portfolio_tool;
pub mod safety;
pub mod types;

pub use backend::{TradingBackend, TradingError};
pub use mock::{FailureInjector, MockTradingBackend};
pub use place_order_tool::PlaceOrderTool;
pub use query_portfolio_tool::QueryPortfolioTool;
pub use safety::{DailyCounter, PendingOrder, RiskLimits, SafetyMode};
pub use types::{
    BalanceSnapshot, CurrencyBalance, OrderAck, OrderKind, OrderSide, OrderStatus, PlaceOrderArgs,
    PortfolioSnapshot, PositionSnapshot, QueryPortfolioArgs, TimeInForce,
};
