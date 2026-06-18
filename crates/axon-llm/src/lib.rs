//! AXON LLM 智能体
//!
//! ReAct 推理 + Tool Calling + 上下文窗口管理 + 三个内置工具（市场分析/投资组合/订单提交）。

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod agent;
pub mod backend;
#[cfg(feature = "backends")]
pub mod backends;
pub mod config;
pub mod context;
pub mod prompt;
pub mod react_agent;
pub mod tools;
pub mod trading;
pub mod types;

#[cfg(feature = "explain")]
pub mod explain;

#[cfg(feature = "python")]
pub mod python;

// ─── 公共导出 ──────────────────────────────────────────────

pub use agent::{AgentConfig, AgentError, ErrorSeverity};
pub use backend::{LLMBackend, LLMError, ToolDefinition};
pub use context::{ContextManager, ConversationMemory};
pub use prompt::PromptTemplate;
pub use react_agent::{AgentResponse, ReActAgent, ReasoningStep};
pub use tools::{Tool, ToolError, ToolResult};
pub use trading::{
    BalanceSnapshot, CurrencyBalance, DailyCounter, FailureInjector, LabeledCounter,
    LatencyHistogram, LatencySample, MetricKind, MetricSample, MockTradingBackend, OrderAck,
    OrderKind, OrderSide, OrderStatus, PlaceOrderArgs, PlaceOrderTool, PortfolioSnapshot,
    PositionSnapshot, QueryPortfolioArgs, QueryPortfolioTool, RiskLimits, RiskRule, SafetyMode,
    TimeInForce, TradingBackend, TradingError, TradingMetrics,
};
pub use types::{FinishReason, LLMResponse, Message, Role, TokenUsage, ToolCall};
