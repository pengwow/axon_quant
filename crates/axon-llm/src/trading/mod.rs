//! 交易工具模块:place_order / query_portfolio 工具与后端抽象
//!
//! 详见 `docs/superpowers/specs/2026-06-16-axon-llm-trading-tools-design.md`。
//!
//! 适配器后端(均为 opt-in feature):
//! - `trading-exchange`:`ExchangeTradingBackend`(本仓库)
//! - 后续 Stage B / C 计划新增 `trading-oms` / `trading-backtest`,同模式扩展。

pub mod backend;
pub mod mock;
pub mod place_order_tool;
pub mod query_portfolio_tool;
pub mod safety;
pub mod types;

#[cfg(feature = "trading-exchange")]
pub mod exchange;

pub use backend::{TradingBackend, TradingError};
pub use mock::{FailureInjector, MockTradingBackend};
pub use place_order_tool::PlaceOrderTool;
pub use query_portfolio_tool::QueryPortfolioTool;
pub use safety::{DailyCounter, PendingOrder, RiskLimits, SafetyMode};
pub use types::{
    BalanceSnapshot, CurrencyBalance, OrderAck, OrderKind, OrderSide, OrderStatus, PlaceOrderArgs,
    PortfolioSnapshot, PositionSnapshot, QueryPortfolioArgs, TimeInForce,
};

#[cfg(feature = "trading-exchange")]
pub use exchange::{ExchangeTradingBackend, SymbolMap};
