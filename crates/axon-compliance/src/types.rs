//! 合规审计核心类型定义

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 交易唯一标识
pub type TradeId = Uuid;

/// 订单唯一标识
pub type OrderId = Uuid;

/// 完整交易记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    /// 交易 ID
    pub trade_id: TradeId,
    /// 订单 ID
    pub order_id: OrderId,
    /// 策略 ID
    pub strategy_id: String,
    /// 交易对
    pub symbol: String,
    /// 交易方向
    pub side: TradeSide,
    /// 数量
    pub quantity: f64,
    /// 价格
    pub price: f64,
    /// 名义价值 (quantity * price)
    pub notional_value: f64,
    /// 手续费
    pub fee: f64,
    /// 手续费货币
    pub fee_currency: String,
    /// 交易所
    pub exchange: String,
    /// 执行时间
    pub execution_time: DateTime<Utc>,
    /// 结算时间
    pub settlement_time: Option<DateTime<Utc>>,
    /// 交易状态
    pub status: TradeStatus,
    /// 订单类型
    pub order_type: OrderType,
    /// 交易所交易 ID
    pub exchange_trade_id: Option<String>,
    /// 流动性类型 (maker/taker)
    pub liquidity: LiquidityType,
    /// 已实现盈亏
    pub realized_pnl: Option<f64>,
    /// 资金费率
    pub funding_rate: Option<f64>,
    /// 滑点
    pub slippage: Option<f64>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
}

/// 交易方向
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TradeSide {
    /// 买入
    Buy,
    /// 卖出
    Sell,
}

/// 交易状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TradeStatus {
    /// 待处理
    Pending,
    /// 已成交
    Filled,
    /// 部分成交
    PartiallyFilled {
        /// 已成交数量
        filled_qty: f64,
    },
    /// 已取消
    Cancelled,
    /// 已拒绝
    Rejected {
        /// 拒绝原因
        reason: String,
    },
}

/// 订单类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderType {
    /// 市价单
    Market,
    /// 限价单
    Limit,
    /// 止损单
    StopLoss,
    /// 止盈单
    TakeProfit,
    /// 止损限价单
    StopLimit,
    /// 追踪止损单
    TrailingStop,
}

/// 流动性类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LiquidityType {
    /// Maker（挂单）
    Maker,
    /// Taker（吃单）
    Taker,
}

/// 审计事件类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuditEventType {
    /// 交易执行
    TradeExecuted,
    /// 订单下单
    OrderPlaced,
    /// 订单取消
    OrderCancelled,
    /// 订单修改
    OrderModified,
    /// 仓位开仓
    PositionOpened,
    /// 仓位平仓
    PositionClosed,
    /// 策略启动
    StrategyStarted,
    /// 策略停止
    StrategyStopped,
    /// 配置变更
    ConfigChanged,
    /// 用户登录
    UserLogin,
    /// 用户登出
    UserLogout,
    /// API Key 创建
    ApiKeyCreated,
    /// API Key 撤销
    ApiKeyRevoked,
    /// 报告生成
    ReportGenerated,
    /// 数据导出
    DataExported,
    /// 系统错误
    SystemError,
    /// 合规告警
    ComplianceAlert,
}

/// 审计事件（不可变）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// 事件 ID
    pub event_id: Uuid,
    /// 事件时间戳
    pub timestamp: DateTime<Utc>,
    /// 事件类型
    pub event_type: AuditEventType,
    /// 操作者
    pub actor: String,
    /// 操作
    pub action: String,
    /// 资源类型
    pub resource_type: String,
    /// 资源 ID
    pub resource_id: String,
    /// 事件详情
    pub details: serde_json::Value,
    /// 前一个事件的哈希（链式结构）
    pub previous_hash: String,
    /// 本事件的哈希
    pub event_hash: String,
    /// IP 地址
    pub ip_address: Option<String>,
    /// 会话 ID
    pub session_id: Option<String>,
}

/// 合规配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceConfig {
    /// 账户 ID
    pub account_id: String,
    /// 基准货币
    pub base_currency: String,
    /// 大额交易阈值
    pub large_trade_threshold: f64,
    /// 持仓限制
    pub position_limit: f64,
    /// 最大持仓集中度
    pub max_portfolio_concentration: f64,
    /// 数据保留年限
    pub data_retention_years: u32,
    /// 监管机构列表
    pub regulators: Vec<String>,
}

/// 交易过滤器
#[derive(Debug, Clone, Default)]
pub struct TradeFilter {
    /// 交易对过滤
    pub symbol: Option<String>,
    /// 策略 ID 过滤
    pub strategy_id: Option<String>,
    /// 交易方向过滤
    pub side: Option<TradeSide>,
    /// 交易状态过滤
    pub status: Option<TradeStatus>,
    /// 开始时间
    pub start_time: Option<DateTime<Utc>>,
    /// 结束时间
    pub end_time: Option<DateTime<Utc>>,
    /// 最小名义价值
    pub min_notional: Option<f64>,
}

/// 交易统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TradeStats {
    /// 总交易数
    pub total_trades: u32,
    /// 总成交量
    pub total_volume: f64,
    /// 总手续费
    pub total_fees: f64,
    /// 盈利交易数
    pub winning_trades: u32,
    /// 亏损交易数
    pub losing_trades: u32,
    /// 胜率
    pub win_rate: f64,
    /// 平均交易规模
    pub avg_trade_size: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trade_record_serialization() {
        let trade = TradeRecord {
            trade_id: Uuid::new_v4(),
            order_id: Uuid::new_v4(),
            strategy_id: "test_strategy".into(),
            symbol: "BTCUSDT".into(),
            side: TradeSide::Buy,
            quantity: 1.0,
            price: 50000.0,
            notional_value: 50000.0,
            fee: 50.0,
            fee_currency: "USDT".into(),
            exchange: "Binance".into(),
            execution_time: Utc::now(),
            settlement_time: None,
            status: TradeStatus::Filled,
            order_type: OrderType::Market,
            exchange_trade_id: None,
            liquidity: LiquidityType::Taker,
            realized_pnl: None,
            funding_rate: None,
            slippage: None,
            created_at: Utc::now(),
        };

        // 序列化
        let json = serde_json::to_string(&trade).unwrap();
        assert!(!json.is_empty());

        // 反序列化
        let deserialized: TradeRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.symbol, "BTCUSDT");
        assert_eq!(deserialized.side, TradeSide::Buy);
    }

    #[test]
    fn test_audit_event_serialization() {
        let event = AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            event_type: AuditEventType::TradeExecuted,
            actor: "test_strategy".into(),
            action: "trade_executed".into(),
            resource_type: "trade".into(),
            resource_id: Uuid::new_v4().to_string(),
            details: serde_json::json!({"symbol": "BTCUSDT"}),
            previous_hash: String::new(),
            event_hash: String::new(),
            ip_address: None,
            session_id: None,
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: AuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type, AuditEventType::TradeExecuted);
    }

    #[test]
    fn test_trade_filter_default() {
        let filter = TradeFilter::default();
        assert!(filter.symbol.is_none());
        assert!(filter.side.is_none());
    }
}
