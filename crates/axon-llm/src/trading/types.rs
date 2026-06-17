//! 交易工具共享数据类型
//!
//! 包括 LLM 输入参数(高阶语义 + extras 兜底)与后端返回结果。

use serde::{Deserialize, Serialize};

// ── PlaceOrderArgs ────────────────────────────────────────

/// PlaceOrderTool 输入参数
///
/// 高阶语义字段(symbol / side / quantity / order_type / price / stop_loss /
/// take_profit / time_in_force) + `extras` 兜底透传字段(client_order_id /
/// 交易所特定参数等)。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct PlaceOrderArgs {
    /// 交易对,例 "BTC-USDT"
    pub symbol: String,
    /// 买卖方向
    pub side: OrderSide,
    /// 下单数量(基础货币)
    pub quantity: f64,
    /// 订单类型,默认 Limit
    #[serde(default)]
    pub order_type: OrderKind,
    /// 价格(Limit 必填,Market 忽略)
    #[serde(default)]
    pub price: Option<f64>,
    /// 止损价
    #[serde(default)]
    pub stop_loss: Option<f64>,
    /// 止盈价
    #[serde(default)]
    pub take_profit: Option<f64>,
    /// 时效,默认 GTC
    #[serde(default)]
    pub time_in_force: TimeInForce,
    /// 兜底透传字段
    #[serde(default)]
    pub extras: serde_json::Value,
}

/// 买卖方向
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum OrderSide {
    /// 买入
    Buy,
    /// 卖出
    Sell,
}

/// 订单类型
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "PascalCase")]
pub enum OrderKind {
    /// 限价单(默认)
    #[default]
    Limit,
    /// 市价单
    Market,
}

/// 时效
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum TimeInForce {
    /// Good Till Cancel(默认)
    #[default]
    GTC,
    /// Immediate or Cancel
    IOC,
    /// Fill or Kill
    FOK,
}

// ── QueryPortfolioArgs ────────────────────────────────────

/// QueryPortfolioTool 输入参数
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct QueryPortfolioArgs {
    /// 可选按 symbol 过滤持仓(balance 不受影响)
    #[serde(default)]
    pub symbol: Option<String>,
}

// ── CancelOrderArgs ──────────────────────────────────────

/// CancelOrderTool 输入参数
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CancelOrderArgs {
    /// 要撤销的订单 ID
    pub order_id: String,
}

// ── ReplaceOrderArgs ─────────────────────────────────────

/// ReplaceOrderTool 输入参数
///
/// 携带完整新参数(PlaceOrderArgs 全部字段),后端负责校验 symbol / side /
/// order_type 与原单是否一致(必须一致),并应用 price / quantity / stop_loss /
/// take_profit 的变化。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ReplaceOrderArgs {
    /// 要修改的订单 ID
    pub order_id: String,
    /// 新参数(完整 PlaceOrderArgs)
    pub new_req: PlaceOrderArgs,
}

// ── 后端结果 ──────────────────────────────────────────────

/// 订单回执(后端返回)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderAck {
    /// 后端唯一订单 ID
    pub order_id: String,
    /// 交易对
    pub symbol: String,
    /// 买卖方向
    pub side: OrderSide,
    /// 下单数量
    pub quantity: f64,
    /// 订单状态
    pub status: OrderStatus,
    /// 响应时间戳(毫秒)
    pub timestamp_ms: i64,
    /// TwoPhase 模式专用:第一次返回 token,第二次带相同 token 才真发
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm_token: Option<String>,
}

/// 订单状态字符串(灵活字符串,允许后端扩展,例 "Filled" / "New" / "Cancelled")
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderStatus(pub String);

/// 余额快照
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BalanceSnapshot {
    /// 各币种余额
    pub currencies: Vec<CurrencyBalance>,
    /// 快照时间戳(毫秒)
    pub as_of_ms: i64,
}

/// 单币种余额
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CurrencyBalance {
    /// 币种,例 "USDT" / "BTC"
    pub currency: String,
    /// 可用余额
    pub free: f64,
    /// 锁定余额(冻结,挂单等)
    pub locked: f64,
}

/// 持仓快照
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PositionSnapshot {
    /// 交易对
    pub symbol: String,
    /// 持仓数量(正=多,负=空)
    pub quantity: f64,
    /// 开仓均价
    pub entry_price: f64,
    /// 浮动盈亏
    pub unrealized_pnl: f64,
    /// 快照时间戳(毫秒)
    pub as_of_ms: i64,
}

/// 投资组合快照(balance + positions)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PortfolioSnapshot {
    /// 余额快照
    pub balance: BalanceSnapshot,
    /// 持仓列表
    pub positions: Vec<PositionSnapshot>,
}

// ── 测试 ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_order_args_serde_roundtrip() {
        let a = PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: OrderKind::Limit,
            price: Some(50_000.0),
            stop_loss: Some(49_000.0),
            take_profit: Some(52_000.0),
            time_in_force: TimeInForce::GTC,
            extras: serde_json::json!({"client_order_id": "abc-123"}),
        };
        let s = serde_json::to_string(&a).unwrap();
        let b: PlaceOrderArgs = serde_json::from_str(&s).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn place_order_args_defaults_applied() {
        // 仅传必填字段
        let json = r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.1}"#;
        let a: PlaceOrderArgs = serde_json::from_str(json).unwrap();
        assert_eq!(a.order_type, OrderKind::Limit);
        assert_eq!(a.time_in_force, TimeInForce::GTC);
        assert_eq!(a.price, None);
        assert_eq!(a.extras, serde_json::Value::Null);
    }

    #[test]
    fn order_side_pascal_case() {
        assert_eq!(serde_json::to_string(&OrderSide::Buy).unwrap(), "\"Buy\"");
        assert_eq!(serde_json::to_string(&OrderSide::Sell).unwrap(), "\"Sell\"");
    }

    #[test]
    fn time_in_force_uppercase() {
        assert_eq!(serde_json::to_string(&TimeInForce::GTC).unwrap(), "\"GTC\"");
        assert_eq!(serde_json::to_string(&TimeInForce::IOC).unwrap(), "\"IOC\"");
    }

    #[test]
    fn query_portfolio_args_default_when_empty_json() {
        let a: QueryPortfolioArgs = serde_json::from_str("{}").unwrap();
        assert_eq!(a, QueryPortfolioArgs::default());
        assert_eq!(a.symbol, None);
    }

    #[test]
    fn order_ack_skip_none_confirm_token() {
        let a = OrderAck {
            order_id: "X-1".into(),
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            status: OrderStatus("Filled".into()),
            timestamp_ms: 1_700_000_000_000,
            confirm_token: None,
        };
        let s = serde_json::to_string(&a).unwrap();
        assert!(!s.contains("confirm_token"), "字段应被 skip: {}", s);
    }

    #[test]
    fn portfolio_snapshot_serde_roundtrip() {
        let p = PortfolioSnapshot {
            balance: BalanceSnapshot {
                currencies: vec![CurrencyBalance {
                    currency: "USDT".into(),
                    free: 1000.0,
                    locked: 0.0,
                }],
                as_of_ms: 1_700_000_000_000,
            },
            positions: vec![PositionSnapshot {
                symbol: "BTC-USDT".into(),
                quantity: 0.1,
                entry_price: 50_000.0,
                unrealized_pnl: 100.0,
                as_of_ms: 1_700_000_000_000,
            }],
        };
        let s = serde_json::to_string(&p).unwrap();
        let b: PortfolioSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(p, b);
    }

    // ── CancelOrderArgs / ReplaceOrderArgs(Stage E)─────────

    /// `CancelOrderArgs` 仅含 order_id
    #[test]
    fn cancel_order_args_serde_roundtrip() {
        let a = CancelOrderArgs {
            order_id: "MOCK-1".into(),
        };
        let s = serde_json::to_string(&a).unwrap();
        assert_eq!(s, r#"{"order_id":"MOCK-1"}"#);
        let back: CancelOrderArgs = serde_json::from_str(&s).unwrap();
        assert_eq!(back.order_id, "MOCK-1");
    }

    /// `ReplaceOrderArgs` 含 order_id + 完整 PlaceOrderArgs
    #[test]
    fn replace_order_args_serde_roundtrip() {
        let a = ReplaceOrderArgs {
            order_id: "MOCK-1".into(),
            new_req: PlaceOrderArgs {
                symbol: "BTC-USDT".into(),
                side: OrderSide::Buy,
                quantity: 0.2,
                order_type: OrderKind::Limit,
                price: Some(51_000.0),
                stop_loss: None,
                take_profit: Some(53_000.0),
                time_in_force: TimeInForce::GTC,
                extras: serde_json::Value::Null,
            },
        };
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.contains("\"order_id\":\"MOCK-1\""));
        assert!(s.contains("\"symbol\":\"BTC-USDT\""));
        assert!(s.contains("\"quantity\":0.2"));
        let back: ReplaceOrderArgs = serde_json::from_str(&s).unwrap();
        assert_eq!(back.order_id, "MOCK-1");
        assert_eq!(back.new_req.symbol, "BTC-USDT");
        assert_eq!(back.new_req.quantity, 0.2);
    }
}
