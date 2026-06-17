//! `OmsTradingBackend`:把 `axon_oms::OrderManager` 适配为 `TradingBackend`。
//!
//! 启用需 `--features trading-oms`,默认不引入 `axon-oms` 依赖。
//!
//! 详见 `docs/superpowers/specs/2026-06-17-axon-oms-mvp-design.md`
//! 与 `docs/superpowers/plans/2026-06-17-axon-llm-oms-adapter.md`。
//!
//! **关键设计**:`OmsTradingBackend::place_order` 只调 `OrderManager::submit`,
//! 不调 `add_fill`。OMS 是订单状态机,实际撮合由 OMS 消费者(撮合引擎 / 交易所
//! webhook / 风控事件)推回 fill。LLM 工具视角的"下单" = "登记"。

// 注:本文件中所有 #[allow(dead_code)] / #[allow(unused_imports)] 都已隐式
// 验证(辅助函数被 OmsTradingBackend impl 用到),无 module-level allow 需要。

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "trading-oms")]
use rust_decimal::Decimal;

#[cfg(feature = "trading-oms")]
use crate::trading::types::{
    BalanceSnapshot, CurrencyBalance, OrderAck, OrderKind, OrderSide, PlaceOrderArgs,
    PositionSnapshot,
};

#[cfg(feature = "trading-oms")]
use axon_oms::{OrderType as OmsOrderType, Side as OmsSide};

/// 当前 unix epoch 毫秒数。
#[cfg(feature = "trading-oms")]
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `f64 -> rust_decimal::Decimal`,用字符串往返以保持精度。
/// NaN/+Inf/-Inf 一律拒绝,避免下游 OMS panic。
#[cfg(feature = "trading-oms")]
fn decimal_from_f64(v: f64) -> Result<Decimal, rust_decimal::Error> {
    if !v.is_finite() {
        return Err(rust_decimal::Error::Underflow);
    }
    Decimal::from_str(&v.to_string())
}

/// `rust_decimal::Decimal -> f64`,精度可能损失
/// (关键金额应保持 `Decimal`,LLM 工具仅在 OrderAck 展示用 f64)。
#[cfg(feature = "trading-oms")]
fn f64_from_decimal(d: Decimal) -> Result<f64, std::num::ParseFloatError> {
    d.to_string().parse::<f64>()
}

// ==================== 类型转换 ====================

/// `PlaceOrderArgs` -> `axon_oms::Order`
///
/// **不能用 `impl TryFrom<PlaceOrderArgs> for Order`** — orphan rule 不允许
/// 在 axon-llm 中为外部类型 Order 实现外部 trait TryFrom(均为 std / axon-oms 定义)。
/// 改用 free function。
///
/// **价格兜底**:Limit 订单要求 price,但 `PlaceOrderArgs` 端 `price: Option<f64>`
/// 语义上 Limit 时必有值,Market 时为 None。我们不强制校验(交给 OMS 自身
/// 业务逻辑,OMS 不区分 Limit/Market 在价格层面的语义,只是字段),`price` 为 None
/// 时转 `Decimal::ZERO`(OMS `Order::new` 要求 price: Decimal)。若调用方传
/// Market + price=None,OMS 内部 price=0 不影响撮合(由 OMS 消费者解释)。
///
/// **idempotency_key**:从 `extras.idempotency_key` 透传(string),缺省 None。
/// 其它 extras 字段(leverage / margin_type 等)忽略(OMS 状态机不消费)。
#[cfg(feature = "trading-oms")]
fn args_to_oms_order(args: &PlaceOrderArgs) -> Result<axon_oms::Order, axon_oms::OmsError> {
    let price_dec = if let Some(p) = args.price {
        decimal_from_f64(p)
            .map_err(|e| axon_oms::OmsError::SerializationError(format!("price: {}", e)))?
    } else {
        Decimal::ZERO
    };
    let qty_dec = decimal_from_f64(args.quantity)
        .map_err(|e| axon_oms::OmsError::SerializationError(format!("quantity: {}", e)))?;

    let oms_order = axon_oms::Order::new(
        args.symbol.clone(),
        match args.side {
            OrderSide::Buy => OmsSide::Buy,
            OrderSide::Sell => OmsSide::Sell,
        },
        match args.order_type {
            OrderKind::Limit => OmsOrderType::Limit,
            OrderKind::Market => OmsOrderType::Market,
        },
        qty_dec,
        price_dec,
    );

    // idempotency_key 从 extras 透传(若存在)
    if let Some(key) = args.extras.get("idempotency_key").and_then(|v| v.as_str()) {
        Ok(oms_order.with_idempotency_key(key.to_string()))
    } else {
        Ok(oms_order)
    }
}

/// `axon_oms::OrderStatus` -> `String` 灵活映射(预留 API,供未来 get_order_status 集成使用)
///
/// 设计:`TradingBackend` 的 `OrderStatus` 是 String 灵活类型,OMS 状态
/// 序列化为字符串。`Filled{..}` 渲染为 `"Filled"`,`PartiallyFilled{..}`
/// 渲染为 `"PartiallyFilled"`,以此类推。
///
/// **当前状态**:Stage B-2/2 的 `OmsTradingBackend::place_order` 直接硬编码
/// `"Submitted"`(OrderManager::submit 后状态固定),未使用本函数。保留为
/// public API 供未来 OMS 消费者需要查 in-flight 订单状态时复用。
#[cfg(feature = "trading-oms")]
#[allow(
    dead_code,
    reason = "Public API 预留,当前 place_order 硬编码 Submitted"
)]
pub fn oms_status_to_string(status: &axon_oms::OrderStatus) -> String {
    match status {
        axon_oms::OrderStatus::New => "New".to_string(),
        axon_oms::OrderStatus::Submitted => "Submitted".to_string(),
        axon_oms::OrderStatus::Acknowledged => "Acknowledged".to_string(),
        axon_oms::OrderStatus::PartiallyFilled { .. } => "PartiallyFilled".to_string(),
        axon_oms::OrderStatus::Filled { .. } => "Filled".to_string(),
        axon_oms::OrderStatus::Cancelled { .. } => "Cancelled".to_string(),
        axon_oms::OrderStatus::Rejected { .. } => "Rejected".to_string(),
    }
}

// ==================== balance / position 转换 ====================

/// `axon_oms::PortfolioSnapshot` -> `BalanceSnapshot`
///
/// PortfolioSnapshot.cash 是 `HashMap<currency, Decimal>`,LLM 端
/// `BalanceSnapshot.currencies` 是 `Vec<CurrencyBalance>`(`free` / `locked`)。
/// OMS portfolio 没有"locked"概念(cash 是单值),locked=0 兜底。
#[cfg(feature = "trading-oms")]
#[allow(dead_code)]
fn oms_portfolio_to_balance_snapshot(
    snap: axon_oms::PortfolioSnapshot,
) -> Result<BalanceSnapshot, axon_oms::OmsError> {
    let mut currencies = Vec::with_capacity(snap.cash.len());
    for (currency, amount) in snap.cash {
        let free = f64_from_decimal(amount).map_err(|e| {
            axon_oms::OmsError::SerializationError(format!("cash.{}: {}", currency, e))
        })?;
        currencies.push(CurrencyBalance {
            currency,
            free,
            locked: 0.0,
        });
    }
    // as_of 转为 ms 时间戳
    let as_of_ms = snap.as_of.timestamp_millis();
    Ok(BalanceSnapshot {
        currencies,
        as_of_ms,
    })
}

/// `axon_oms::Position` -> `PositionSnapshot`
///
/// `Position.avg_price` -> `entry_price`;`Position.realized_pnl` 不进
/// `PositionSnapshot`(后者只有 `unrealized_pnl`);`Position.quantity`
/// 已经是带符号,直接 f64 转换。
#[cfg(feature = "trading-oms")]
#[allow(dead_code)]
fn oms_position_to_snapshot(
    pos: axon_oms::Position,
) -> Result<PositionSnapshot, axon_oms::OmsError> {
    let quantity = f64_from_decimal(pos.quantity)
        .map_err(|e| axon_oms::OmsError::SerializationError(format!("quantity: {}", e)))?;
    let entry_price = f64_from_decimal(pos.avg_price)
        .map_err(|e| axon_oms::OmsError::SerializationError(format!("avg_price: {}", e)))?;
    let as_of_ms = pos.updated_at.timestamp_millis();
    Ok(PositionSnapshot {
        symbol: pos.symbol,
        quantity,
        entry_price,
        // unrealized_pnl OMS 不跟踪(OMS 是状态机,mark-to-market 由外部做),
        // 兜底 0.0
        unrealized_pnl: 0.0,
        as_of_ms,
    })
}

// ==================== 错误映射 ====================

#[cfg(feature = "trading-oms")]
use crate::trading::backend::TradingError;

/// `OmsError` -> `TradingError` 映射。
///
/// 映射原则:
/// - 业务层(订单被拒 / 限频 / 状态机不合法)→ `Backend` 带前缀
/// - 协议层(序列化 / 网络 / 恢复失败)→ `Backend`
/// - 重复 idempotency key → `Backend`(用户可见,带原 key)
/// - Portfolio 错误(Stage B-MVP 是 String 包装)→ `Backend` 透传
#[cfg(feature = "trading-oms")]
fn map_oms_error(e: axon_oms::OmsError) -> TradingError {
    match e {
        axon_oms::OmsError::OrderNotFound(id) => {
            TradingError::Backend(format!("order not found: {}", id))
        }
        axon_oms::OmsError::InvalidTransition { from, to } => {
            TradingError::Backend(format!("invalid transition: {} -> {}", from, to))
        }
        axon_oms::OmsError::DuplicateIdempotencyKey(key) => {
            TradingError::Backend(format!("duplicate idempotency key: {}", key))
        }
        axon_oms::OmsError::AlreadyTerminal(id) => {
            TradingError::Backend(format!("order already in terminal state: {}", id))
        }
        axon_oms::OmsError::ExchangeRejected(reason) => {
            TradingError::Backend(format!("exchange rejected: {}", reason))
        }
        axon_oms::OmsError::NetworkError(msg) => {
            TradingError::Backend(format!("network error: {}", msg))
        }
        axon_oms::OmsError::SerializationError(msg) => {
            TradingError::Backend(format!("serialization error: {}", msg))
        }
        axon_oms::OmsError::RecoveryFailed(msg) => {
            TradingError::Backend(format!("recovery failed: {}", msg))
        }
        axon_oms::OmsError::Portfolio(msg) => {
            TradingError::Backend(format!("portfolio error: {}", msg))
        }
    }
}

// ==================== OmsTradingBackend ====================

#[cfg(feature = "trading-oms")]
use crate::trading::backend::TradingBackend;
#[cfg(feature = "trading-oms")]
use crate::trading::types::OrderStatus as LlOrderStatus;
#[cfg(feature = "trading-oms")]
use async_trait::async_trait;
#[cfg(feature = "trading-oms")]
use std::sync::Arc;

/// OMS 交易后端:包装 `OrderManager` 提供 `TradingBackend` 接口。
///
/// **关键设计**:
/// - `place_order` 调 `OrderManager::submit`,**不**调 `add_fill`。
///   OMS 是状态机,实际撮合由 OMS 消费者(撮合引擎 / 交易所 webhook)推回 fill。
/// - `get_balance` / `get_positions` 读 OMS 内嵌 portfolio 状态。
/// - `OrderAck.status` 永远是 `"Submitted"`(submit 后状态)。
/// - 锁:`OrderManager` 内部锁(parking_lot RwLock)保证线程安全,本 wrapper 无额外锁。
#[cfg(feature = "trading-oms")]
pub struct OmsTradingBackend {
    manager: Arc<axon_oms::OrderManager>,
}

#[cfg(feature = "trading-oms")]
impl OmsTradingBackend {
    /// 包装一个 `OrderManager`。
    ///
    /// 建议在传入前 `manager.deposit()` 设置初始 cash(否则 buy 会被 portfolio 拒)。
    pub fn new(manager: Arc<axon_oms::OrderManager>) -> Self {
        Self { manager }
    }

    /// 当前底层 manager 的 Arc 引用(供外部推 fill / 撤单 / 查历史等)。
    pub fn manager(&self) -> Arc<axon_oms::OrderManager> {
        self.manager.clone()
    }
}

#[cfg(feature = "trading-oms")]
#[async_trait]
impl TradingBackend for OmsTradingBackend {
    async fn place_order(&self, req: &PlaceOrderArgs) -> Result<OrderAck, TradingError> {
        // 1. PlaceOrderArgs -> OMS Order
        let oms_order = args_to_oms_order(req).map_err(map_oms_error)?;

        // 2. 调 OMS submit(返回 OrderId,status 自动转 Submitted)
        let order_id = self.manager.submit(oms_order).map_err(map_oms_error)?;

        // 3. OrderAck 字段填充
        Ok(OrderAck {
            order_id: order_id.to_string(),
            symbol: req.symbol.clone(),
            side: req.side,
            quantity: req.quantity,
            status: LlOrderStatus("Submitted".into()),
            timestamp_ms: now_ms(),
            confirm_token: None,
        })
    }

    async fn get_balance(&self) -> Result<BalanceSnapshot, TradingError> {
        let snap = self.manager.snapshot_balance();
        oms_portfolio_to_balance_snapshot(snap).map_err(map_oms_error)
    }

    async fn get_positions(&self) -> Result<Vec<PositionSnapshot>, TradingError> {
        let positions = self.manager.snapshot_positions();
        positions
            .into_iter()
            .map(oms_position_to_snapshot)
            .collect::<Result<Vec<_>, axon_oms::OmsError>>()
            .map_err(map_oms_error)
    }
}

#[cfg(all(test, feature = "trading-oms"))]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use serde_json::json;

    // ==================== 辅助函数测试 ====================

    #[test]
    fn decimal_from_f64_converts_finite_values() {
        // 验证:正常 f64 -> Decimal
        let d = decimal_from_f64(1.5).unwrap();
        assert_eq!(d, Decimal::from_str("1.5").unwrap());
    }

    #[test]
    fn decimal_from_f64_rejects_nan() {
        // 验证:NaN 拒绝
        assert!(decimal_from_f64(f64::NAN).is_err());
    }

    #[test]
    fn decimal_from_f64_rejects_infinity() {
        // 验证:+Inf / -Inf 拒绝
        assert!(decimal_from_f64(f64::INFINITY).is_err());
        assert!(decimal_from_f64(f64::NEG_INFINITY).is_err());
    }

    #[test]
    fn f64_from_decimal_round_trip() {
        // 验证:Decimal -> f64 -> Decimal 应尽量保持精度
        let d = dec!(0.001);
        let f = f64_from_decimal(d).unwrap();
        let d2 = decimal_from_f64(f).unwrap();
        assert_eq!(d, d2);
    }

    #[test]
    fn now_ms_returns_positive_unix_millis() {
        // 验证:now_ms() 返回正整数(unix epoch ms)
        let t = now_ms();
        assert!(t > 0);
        // 2026-01-01 之后的 ms:约 1.76e12
        assert!(t > 1_700_000_000_000);
    }

    // ==================== 类型转换测试 ====================

    fn make_args(side: OrderSide, qty: f64, price: Option<f64>) -> PlaceOrderArgs {
        PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side,
            quantity: qty,
            order_type: OrderKind::Limit,
            price,
            stop_loss: None,
            take_profit: None,
            time_in_force: crate::trading::types::TimeInForce::GTC,
            extras: json!({}),
        }
    }

    #[test]
    fn args_to_oms_order_translates_side() {
        // 验证:Buy -> Buy / Sell -> Sell
        let args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        let oms = args_to_oms_order(&args).unwrap();
        assert_eq!(oms.side, OmsSide::Buy);

        let args = make_args(OrderSide::Sell, 1.0, Some(50000.0));
        let oms = args_to_oms_order(&args).unwrap();
        assert_eq!(oms.side, OmsSide::Sell);
    }

    #[test]
    fn args_to_oms_order_translates_order_type() {
        // 验证:Limit -> Limit / Market -> Market
        let args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        let oms = args_to_oms_order(&args).unwrap();
        assert_eq!(oms.order_type, OmsOrderType::Limit);

        let args = PlaceOrderArgs {
            order_type: OrderKind::Market,
            price: None,
            ..make_args(OrderSide::Buy, 1.0, None)
        };
        let oms = args_to_oms_order(&args).unwrap();
        assert_eq!(oms.order_type, OmsOrderType::Market);
    }

    #[test]
    fn args_to_oms_order_converts_decimal_fields() {
        // 验证:quantity / price 转 Decimal
        let args = make_args(OrderSide::Buy, 0.5, Some(50000.0));
        let oms = args_to_oms_order(&args).unwrap();
        assert_eq!(oms.quantity, dec!(0.5));
        assert_eq!(oms.price, dec!(50000));
    }

    #[test]
    fn args_to_oms_order_market_price_defaults_to_zero() {
        // 验证:Market + price=None 时,OMS 端 price=0(OMS 不强制要求 Limit 价格)
        let args = PlaceOrderArgs {
            order_type: OrderKind::Market,
            price: None,
            ..make_args(OrderSide::Buy, 1.0, None)
        };
        let oms = args_to_oms_order(&args).unwrap();
        assert_eq!(oms.price, Decimal::ZERO);
    }

    #[test]
    fn args_to_oms_order_passes_symbol() {
        // 验证:symbol 透传
        let args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        let oms = args_to_oms_order(&args).unwrap();
        assert_eq!(oms.instrument_id, "BTC-USDT");
    }

    #[test]
    fn args_to_oms_order_passes_idempotency_key_from_extras() {
        // 验证:extras.idempotency_key 透传
        let mut args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        args.extras = json!({ "idempotency_key": "test-key-123" });
        let oms = args_to_oms_order(&args).unwrap();
        assert_eq!(oms.idempotency_key, Some("test-key-123".to_string()));
    }

    #[test]
    fn args_to_oms_order_no_idempotency_key_when_extras_empty() {
        // 验证:extras 无 idempotency_key 时,OMS 端为 None
        let args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        let oms = args_to_oms_order(&args).unwrap();
        assert_eq!(oms.idempotency_key, None);
    }

    #[test]
    fn args_to_oms_order_rejects_nan_quantity() {
        // 验证:NaN quantity 失败
        let args = make_args(OrderSide::Buy, f64::NAN, Some(50000.0));
        assert!(args_to_oms_order(&args).is_err());
    }

    // ==================== OrderStatus 字符串映射测试 ====================

    #[test]
    fn oms_status_to_string_submitted() {
        // 验证:Submitted -> "Submitted"
        assert_eq!(
            oms_status_to_string(&axon_oms::OrderStatus::Submitted),
            "Submitted"
        );
    }

    #[test]
    fn oms_status_to_string_filled() {
        // 验证:Filled{..} -> "Filled"
        let s = axon_oms::OrderStatus::Filled {
            filled_qty: dec!(1),
            avg_price: dec!(50000),
        };
        assert_eq!(oms_status_to_string(&s), "Filled");
    }

    #[test]
    fn oms_status_to_string_rejected() {
        // 验证:Rejected{..} -> "Rejected"
        let s = axon_oms::OrderStatus::Rejected {
            reason: "test".into(),
        };
        assert_eq!(oms_status_to_string(&s), "Rejected");
    }

    #[test]
    fn oms_status_to_string_cancelled() {
        // 验证:Cancelled{..} -> "Cancelled"
        let s = axon_oms::OrderStatus::Cancelled {
            filled_qty: dec!(0),
        };
        assert_eq!(oms_status_to_string(&s), "Cancelled");
    }

    // ==================== balance / position 转换测试 ====================

    fn oms_balance_snapshot() -> axon_oms::PortfolioSnapshot {
        use chrono::Utc;
        axon_oms::PortfolioSnapshot {
            cash: std::collections::HashMap::new(),
            positions: Vec::new(),
            as_of: Utc::now(),
        }
    }

    #[test]
    fn oms_portfolio_to_balance_snapshot_aggregates_currencies() {
        // 验证:HashMap<currency, Decimal> -> Vec<CurrencyBalance>
        use chrono::Utc;
        use std::collections::HashMap;
        let mut cash = HashMap::new();
        cash.insert("USDT".to_string(), dec!(1000));
        cash.insert("BTC".to_string(), dec!(0.5));
        let snap = axon_oms::PortfolioSnapshot {
            cash,
            positions: Vec::new(),
            as_of: Utc::now(),
        };
        let bal = oms_portfolio_to_balance_snapshot(snap).unwrap();
        assert_eq!(bal.currencies.len(), 2);
        let usdt = bal
            .currencies
            .iter()
            .find(|c| c.currency == "USDT")
            .unwrap();
        assert!((usdt.free - 1000.0).abs() < 1e-9);
        assert!((usdt.locked - 0.0).abs() < 1e-9);
        let btc = bal.currencies.iter().find(|c| c.currency == "BTC").unwrap();
        assert!((btc.free - 0.5).abs() < 1e-9);
    }

    #[test]
    fn oms_portfolio_to_balance_snapshot_empty_cash() {
        // 验证:空 cash 转换成功
        let bal = oms_portfolio_to_balance_snapshot(oms_balance_snapshot()).unwrap();
        assert_eq!(bal.currencies.len(), 0);
    }

    #[test]
    fn oms_position_to_snapshot_long_position() {
        // 验证:正 quantity -> long
        use chrono::Utc;
        let pos = axon_oms::Position {
            symbol: "BTC-USDT".into(),
            quantity: dec!(0.5),
            avg_price: dec!(50000),
            realized_pnl: dec!(0),
            updated_at: Utc::now(),
        };
        let snap = oms_position_to_snapshot(pos).unwrap();
        assert_eq!(snap.symbol, "BTC-USDT");
        assert!((snap.quantity - 0.5).abs() < 1e-9);
        assert!((snap.entry_price - 50000.0).abs() < 1e-9);
        assert!((snap.unrealized_pnl - 0.0).abs() < 1e-9);
    }

    #[test]
    fn oms_position_to_snapshot_short_position() {
        // 验证:负 quantity -> short
        use chrono::Utc;
        let pos = axon_oms::Position {
            symbol: "BTC-USDT".into(),
            quantity: dec!(-0.3),
            avg_price: dec!(51000),
            realized_pnl: dec!(0),
            updated_at: Utc::now(),
        };
        let snap = oms_position_to_snapshot(pos).unwrap();
        assert!((snap.quantity - (-0.3)).abs() < 1e-9);
        assert!((snap.entry_price - 51000.0).abs() < 1e-9);
    }

    // ==================== map_oms_error 测试 ====================

    #[test]
    fn map_oms_error_order_not_found_includes_id() {
        // 验证:OrderNotFound 转换保留 id
        let e = axon_oms::OmsError::OrderNotFound("order-123".into());
        let mapped = map_oms_error(e);
        match mapped {
            TradingError::Backend(msg) => assert!(msg.contains("order-123")),
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    #[test]
    fn map_oms_error_invalid_transition_includes_from_to() {
        // 验证:InvalidTransition 转换保留 from/to
        let e = axon_oms::OmsError::InvalidTransition {
            from: "New".into(),
            to: "Filled".into(),
        };
        let mapped = map_oms_error(e);
        match mapped {
            TradingError::Backend(msg) => {
                assert!(msg.contains("New"));
                assert!(msg.contains("Filled"));
            }
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    #[test]
    fn map_oms_error_duplicate_idempotency_key_includes_key() {
        // 验证:DuplicateIdempotencyKey 转换保留 key
        let e = axon_oms::OmsError::DuplicateIdempotencyKey("test-key-456".into());
        let mapped = map_oms_error(e);
        match mapped {
            TradingError::Backend(msg) => assert!(msg.contains("test-key-456")),
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    #[test]
    fn map_oms_error_portfolio_passes_through() {
        // 验证:Portfolio 错误透传
        let e = axon_oms::OmsError::Portfolio("insufficient cash: need 100 USDT, have 50".into());
        let mapped = map_oms_error(e);
        match mapped {
            TradingError::Backend(msg) => assert!(msg.contains("insufficient cash")),
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    // ==================== OmsTradingBackend struct 单测 ====================

    #[test]
    fn oms_trading_backend_new_keeps_arc() {
        // 验证:new() 持有 Arc 引用,manager() 返回克隆共享同一指针
        let manager = Arc::new(axon_oms::OrderManager::new());
        let backend = OmsTradingBackend::new(manager.clone());
        let m1 = backend.manager();
        let m2 = backend.manager();
        // 两次 Arc::clone 共享同一指针
        assert!(Arc::ptr_eq(&m1, &m2));
    }

    // ==================== TradingBackend impl 集成测试(基础 OMS + 业务) ====================

    #[tokio::test]
    async fn place_order_returns_ack_with_oms_order_id() {
        // 验证:place_order 成功时,OrderAck 字段来自 OMS submit 返回
        let manager = Arc::new(axon_oms::OrderManager::new());
        manager.deposit("USDT", dec!(10000));
        let backend = OmsTradingBackend::new(manager.clone());
        let args = make_args(OrderSide::Buy, 0.001, Some(50000.0));
        let ack = backend.place_order(&args).await.unwrap();
        assert_eq!(ack.symbol, "BTC-USDT");
        assert_eq!(ack.side, OrderSide::Buy);
        assert!((ack.quantity - 0.001).abs() < 1e-9);
        // OMS submit 后状态是 Submitted
        assert_eq!(ack.status.0, "Submitted");
        // OrderId(Uuid) -> string 非空
        assert!(!ack.order_id.is_empty());
        // OMS active_orders 增 1
        assert_eq!(manager.active_count(), 1);
    }

    #[tokio::test]
    async fn place_order_market_status_submitted() {
        // 验证:Market 订单 submit 后 status="Submitted"
        let manager = Arc::new(axon_oms::OrderManager::new());
        manager.deposit("USDT", dec!(10000));
        let backend = OmsTradingBackend::new(manager);
        let args = PlaceOrderArgs {
            order_type: OrderKind::Market,
            price: None,
            ..make_args(OrderSide::Buy, 0.1, None)
        };
        let ack = backend.place_order(&args).await.unwrap();
        assert_eq!(ack.status.0, "Submitted");
    }

    #[tokio::test]
    async fn place_order_idempotency_key_from_extras_dedupes() {
        // 验证:相同 idempotency_key 第二次 place_order 被 OMS 拒绝(DuplicateIdempotencyKey)
        let manager = Arc::new(axon_oms::OrderManager::new());
        manager.deposit("USDT", dec!(10000));
        let backend = OmsTradingBackend::new(manager.clone());
        let mut args = make_args(OrderSide::Buy, 0.001, Some(50000.0));
        args.extras = json!({ "idempotency_key": "test-key-1" });
        backend.place_order(&args).await.unwrap();
        // 第二次相同 key 应被 OMS 拒
        let result = backend.place_order(&args).await;
        match result {
            Err(TradingError::Backend(msg)) => {
                assert!(msg.contains("duplicate idempotency key"));
            }
            other => panic!("expected Backend error, got {:?}", other),
        }
        // OMS 仍只有 1 个 active order(第二次被拒)
        assert_eq!(manager.active_count(), 1);
    }

    #[tokio::test]
    async fn get_balance_reflects_deposit() {
        // 验证:get_balance 反映 deposit 后的 cash
        let manager = Arc::new(axon_oms::OrderManager::new());
        manager.deposit("USDT", dec!(10000));
        manager.deposit("BTC", dec!(1));
        let backend = OmsTradingBackend::new(manager);
        let bal = backend.get_balance().await.unwrap();
        assert_eq!(bal.currencies.len(), 2);
        let usdt = bal
            .currencies
            .iter()
            .find(|c| c.currency == "USDT")
            .unwrap();
        assert!((usdt.free - 10000.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn get_positions_empty_initially() {
        // 验证:初始无 fill 时,get_positions 返回空 Vec
        let manager = Arc::new(axon_oms::OrderManager::new());
        let backend = OmsTradingBackend::new(manager);
        let positions = backend.get_positions().await.unwrap();
        assert!(positions.is_empty());
    }

    #[tokio::test]
    async fn get_positions_reflects_fills() {
        // 验证:place_order + OMS add_fill 后,get_positions 反映持仓
        let manager = Arc::new(axon_oms::OrderManager::new());
        // deposit 100000 USDT(0.5 * 50000 = 25000,留 4x 余量)
        manager.deposit("USDT", dec!(100000));
        let backend = OmsTradingBackend::new(manager.clone());
        let args = make_args(OrderSide::Buy, 0.5, Some(50000.0));
        let ack = backend.place_order(&args).await.unwrap();
        let order_id = axon_oms::OrderId(uuid::Uuid::parse_str(&ack.order_id).unwrap());
        // OMS 状态机走 Acknowledged -> 推 fill
        manager
            .update_status(order_id, axon_oms::OrderStatus::Acknowledged)
            .unwrap();
        manager
            .add_fill(
                order_id,
                axon_oms::Fill {
                    fill_id: "f1".into(),
                    symbol: "BTC-USDT".into(),
                    price: dec!(50000),
                    quantity: dec!(0.5),
                    fee: dec!(0),
                    timestamp: chrono::Utc::now(),
                },
            )
            .unwrap();
        let positions = backend.get_positions().await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, "BTC-USDT");
        assert!((positions[0].quantity - 0.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn get_balance_decreases_after_fill() {
        // 验证:place_order + add_fill 后,get_balance cash 减少
        let manager = Arc::new(axon_oms::OrderManager::new());
        // deposit 100000 USDT(fill 0.5 * 50000 = 25000,留 4x 余量)
        manager.deposit("USDT", dec!(100000));
        let backend = OmsTradingBackend::new(manager.clone());
        let args = make_args(OrderSide::Buy, 0.5, Some(50000.0));
        let ack = backend.place_order(&args).await.unwrap();
        let order_id = axon_oms::OrderId(uuid::Uuid::parse_str(&ack.order_id).unwrap());
        manager
            .update_status(order_id, axon_oms::OrderStatus::Acknowledged)
            .unwrap();
        manager
            .add_fill(
                order_id,
                axon_oms::Fill {
                    fill_id: "f1".into(),
                    symbol: "BTC-USDT".into(),
                    price: dec!(50000),
                    quantity: dec!(0.5),
                    fee: dec!(0),
                    timestamp: chrono::Utc::now(),
                },
            )
            .unwrap();
        let bal = backend.get_balance().await.unwrap();
        // balance 接口不 panic,具体值由 OMS 决定
        assert_eq!(bal.currencies.len(), 1);
        // cash 应减少:100000 - 25000 = 75000
        let usdt = bal
            .currencies
            .iter()
            .find(|c| c.currency == "USDT")
            .unwrap();
        assert!((usdt.free - 75000.0).abs() < 1e-9);
    }
}
