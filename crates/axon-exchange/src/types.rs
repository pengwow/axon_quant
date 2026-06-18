use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExchangeId {
    Binance,
    Okx,
}

impl fmt::Display for ExchangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExchangeId::Binance => write!(f, "binance"),
            ExchangeId::Okx => write!(f, "okx"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Symbol(pub String);

impl Symbol {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderId(pub Uuid);

impl OrderId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for OrderId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    Limit,
    Market,
    StopLoss,
    StopLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub client_order_id: OrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub order_type: OrderType,
    pub price: Option<Decimal>,
    pub quantity: Decimal,
    pub time_in_force: TimeInForce,
    pub exchange: ExchangeId,
    pub meta: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Sent,
    Acknowledged,
    PartiallyFilled {
        filled_qty: Decimal,
        avg_price: Decimal,
    },
    Filled {
        filled_qty: Decimal,
        avg_price: Decimal,
    },
    Cancelled {
        filled_qty: Decimal,
    },
    Rejected {
        reason: String,
    },
}

impl OrderStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OrderStatus::Filled { .. }
                | OrderStatus::Cancelled { .. }
                | OrderStatus::Rejected { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticker {
    pub symbol: Symbol,
    pub bid: Decimal,
    pub ask: Decimal,
    pub last: Decimal,
    pub volume_24h: Decimal,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kline {
    pub symbol: Symbol,
    pub interval: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub timestamp: DateTime<Utc>,
    pub is_closed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub symbol: Symbol,
    pub price: Decimal,
    pub quantity: Decimal,
    pub side: Side,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthSnapshot {
    pub symbol: Symbol,
    pub bids: Vec<(Decimal, Decimal)>,
    pub asks: Vec<(Decimal, Decimal)>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderUpdate {
    pub order_id: String,
    pub client_order_id: OrderId,
    pub status: OrderStatus,
    pub filled_qty: Decimal,
    pub avg_price: Option<Decimal>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WsMessage {
    Ticker(Ticker),
    Kline(Kline),
    Trade(Trade),
    Depth(DepthSnapshot),
    OrderUpdate(OrderUpdate),
    Ping,
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeConfig {
    pub exchange_id: ExchangeId,
    pub api_key: String,
    pub api_secret: String,
    pub passphrase: Option<String>,
    pub testnet: bool,
    pub rest_base_url: String,
    pub ws_url: String,
    pub rate_limit: RateLimitConfig,
    pub reconnect: ReconnectConfig,
    /// 代理地址，None 时使用系统环境变量（https_proxy / http_proxy）
    #[serde(default)]
    pub proxy: Option<String>,
    /// 持仓查询 REST 端点。
    /// - Binance 合约：`/fapi/v2/positionRisk`（默认）
    /// - OKX：`/api/v5/account/positions`
    /// - Binance 现货（不支持期货时）：留空则回退为空 Vec
    #[serde(default = "default_position_endpoint")]
    pub position_endpoint: String,
    /// 合约 REST API 基础 URL（Binance USDⓈ-M 专用）。
    /// - 默认：`https://fapi.binance.com`（生产）或 `https://testnet.binancefuture.com`（测试网，根据 `testnet` 推断）
    /// - OKX 忽略此字段（OKX 合约与现货共享 `rest_base_url`）
    #[serde(default)]
    pub fapi_base_url: Option<String>,
}

fn default_position_endpoint() -> String {
    "/fapi/v2/positionRisk".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub requests_per_second: u32,
    pub orders_per_minute: u32,
    pub ws_messages_per_second: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectConfig {
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_multiplier: f64,
    pub circuit_breaker_threshold: u32,
    pub circuit_breaker_reset: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountBalance {
    pub currency: String,
    pub available: Decimal,
    pub locked: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: Symbol,
    pub side: Side,
    pub quantity: Decimal,
    pub avg_entry_price: Decimal,
    pub unrealized_pnl: Decimal,
}

// ============================================================================
// 杠杆 / 合约相关类型(Stage 4' D 新增,生产就绪)
// ============================================================================

/// 保证金模式(合约专用)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarginType {
    /// 逐仓:单 symbol 独立保证金
    Isolated,
    /// 全仓:共享账户余额
    Cross,
}

/// 持仓模式(对冲 vs 单向)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionMode {
    /// 单向模式(一个 symbol 一个净持仓)
    Net,
    /// 对冲模式(多空分别持仓)
    Hedge,
}

/// 杠杆分层上限(每个 symbol 的最大名义价值)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeverageBracket {
    /// 层级编号(从 1 开始)
    pub bracket: u32,
    /// 该层最小杠杆(固定 1)
    pub min_leverage: u8,
    /// 该层最大杠杆
    pub max_leverage: u8,
    /// 该层最大名义价值(USD)
    pub max_notional: Decimal,
    /// 该层最低保证金率
    pub maint_margin_ratio: Decimal,
}

/// 资金费率(永续合约 8h 结算)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingRate {
    /// 合约 symbol
    pub symbol: String,
    /// 资金费率(0.0001 = 0.01%)
    pub rate: Decimal,
    /// 下次结算毫秒时间戳
    pub next_funding_ms: i64,
    /// 标记价格
    pub mark_price: Decimal,
    /// 指数价格
    pub index_price: Decimal,
}

/// 全账户信息(余额 + 盈亏 + 保证金)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    /// 总余额(含未实现盈亏)
    pub total_balance: Decimal,
    /// 可用余额
    pub available_balance: Decimal,
    /// 未实现盈亏
    pub unrealized_pnl: Decimal,
    /// 已占用保证金
    pub margin_used: Decimal,
    /// 初始保证金
    pub initial_margin: Decimal,
    /// 维持保证金
    pub maintenance_margin: Decimal,
    /// 持仓模式
    pub position_mode: PositionMode,
    /// 快照毫秒时间戳
    pub as_of_ms: i64,
}

/// 未平仓合约数(市场情绪指标)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenInterest {
    pub symbol: String,
    /// 未平仓合约张数
    pub contracts: u64,
    /// 美元名义价值
    pub notional: Decimal,
    pub timestamp_ms: i64,
}

/// 多空账户比(主动买入/卖出成交量比,市场情绪指标)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongShortRatio {
    pub symbol: String,
    /// 多仓账户占比 0.0~1.0
    pub long_ratio: f64,
    /// 空仓账户占比 0.0~1.0
    pub short_ratio: f64,
    /// 多/空 比值
    pub long_short_ratio: f64,
    pub timestamp_ms: i64,
}
