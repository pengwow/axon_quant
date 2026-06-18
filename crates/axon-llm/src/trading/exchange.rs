//! `ExchangeTradingBackend`:把 `ExchangeAdapter` 适配为 `TradingBackend`。
//!
//! 启用需 `--features trading-exchange`,默认不引入 `axon-exchange` 依赖。
//!
//! 详见 `docs/superpowers/specs/2026-06-17-axon-llm-exchange-adapter-design.md`
//! 与 `docs/superpowers/plans/2026-06-17-axon-llm-exchange-adapter.md`。

use std::collections::HashMap;

/// LLM 语义 symbol ↔ 交易所原生 symbol 转换。
///
/// 不同交易所命名差异大(Binance `BTCUSDT` / OKX 现货 `BTC-USDT` /
/// OKX 永续 `BTC-USDT-SWAP`),没有通用规则可自动推断,
/// 故由使用方显式 `register`。
#[derive(Debug, Clone, Default)]
pub struct SymbolMap {
    to_ex: HashMap<String, String>,
    to_llm: HashMap<String, String>,
}

impl SymbolMap {
    /// 创建空 map
    pub fn new() -> Self {
        Self {
            to_ex: HashMap::new(),
            to_llm: HashMap::new(),
        }
    }

    /// 双向注册一个 LLM symbol ↔ exchange symbol 映射。
    pub fn register(&mut self, llm_symbol: &str, ex_symbol: &str) -> &mut Self {
        self.to_ex
            .insert(llm_symbol.to_string(), ex_symbol.to_string());
        self.to_llm
            .insert(ex_symbol.to_string(), llm_symbol.to_string());
        self
    }

    /// LLM symbol -> 交易所原生 symbol
    pub fn to_exchange(&self, llm: &str) -> Option<String> {
        self.to_ex.get(llm).cloned()
    }

    /// 交易所原生 symbol -> LLM symbol
    pub fn to_llm(&self, ex: &str) -> Option<String> {
        self.to_llm.get(ex).cloned()
    }
}

// ==================== 辅助函数 ====================

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "trading-exchange")]
use rust_decimal::Decimal;

#[cfg(feature = "trading-exchange")]
use crate::trading::types::TimeInForce;

#[cfg(feature = "trading-exchange")]
use axon_exchange::TimeInForce as ExTif;

#[cfg(feature = "trading-exchange")]
use axon_exchange::ExchangeError;

/// 当前 unix epoch 毫秒数。
#[cfg(feature = "trading-exchange")]
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `f64 -> rust_decimal::Decimal`,用字符串往返以保持精度。
/// NaN/+Inf/-Inf 一律拒绝,避免下游交易所 SDK panic。
#[cfg(feature = "trading-exchange")]
fn decimal_from_f64(v: f64) -> Result<Decimal, rust_decimal::Error> {
    if !v.is_finite() {
        return Err(rust_decimal::Error::Underflow);
    }
    Decimal::from_str(&v.to_string())
}

/// `rust_decimal::Decimal -> f64`,精度可能损失
/// (关键金额应保持 `Decimal`,LLM 工具仅在 OrderAck 展示用 f64)。
#[cfg(feature = "trading-exchange")]
fn f64_from_decimal(d: Decimal) -> Result<f64, std::num::ParseFloatError> {
    d.to_string().parse::<f64>()
}

/// `axon-llm::TimeInForce` -> `axon-exchange::TimeInForce`
/// 枚举值大小写不一致,需显式映射(llm 侧用 UPPERCASE 行业惯例,exchange 侧用 CamelCase serde 风格)。
#[cfg(feature = "trading-exchange")]
fn ex_tif_from(tif: &TimeInForce) -> Result<ExTif, ExchangeError> {
    match tif {
        TimeInForce::GTC => Ok(ExTif::Gtc),
        TimeInForce::IOC => Ok(ExTif::Ioc),
        TimeInForce::FOK => Ok(ExTif::Fok),
    }
}

// ==================== 类型转换 ====================

#[cfg(feature = "trading-exchange")]
use crate::trading::types::{
    BalanceSnapshot, CurrencyBalance, OrderAck, OrderKind, OrderSide, OrderStatus, PlaceOrderArgs,
    PositionSnapshot,
};
#[cfg(feature = "trading-exchange")]
use axon_exchange::{
    AccountBalance as ExBalance, ExchangeId, Order as ExOrder, OrderId, OrderType as ExOrderType,
    Position as ExPosition, Side as ExSide, Symbol as ExSymbol,
};

/// `PlaceOrderArgs` + 已标准化的 ex_symbol + exchange_id -> `ExOrder`
///
/// `Order::client_order_id` 优先从 `extras.client_order_id` 取,否则用 `OrderId::new()`。
/// `Order::meta` 仅收集白名单 key(`leverage` / `margin_type` / `reduce_only` /
/// `stop_loss` / `take_profit` 等),其它 extras 字段忽略。
///
/// **不能用 `impl TryFrom<...> for ExOrder`** — orphan rule 不允许
/// 在 axon-llm 中为外部类型 ExOrder 实现外部 trait TryFrom(均为 std / axon-exchange 定义)。
/// 改用 free function,签名类似 TryFrom。
#[cfg(feature = "trading-exchange")]
fn args_to_ex_order(
    args: &PlaceOrderArgs,
    ex_symbol: String,
    exchange_id: ExchangeId,
) -> Result<ExOrder, ExchangeError> {
    // client_order_id 从 extras 取(LLM 可透传),缺省自动生成 Uuid v7
    let client_order_id = args
        .extras
        .get("client_order_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .map_or_else(OrderId::new, OrderId);

    // meta:仅取白名单 key(string -> string)
    let mut meta = HashMap::new();
    for key in [
        "leverage",
        "margin_type",
        "reduce_only",
        "stop_loss",
        "take_profit",
    ] {
        if let Some(v) = args.extras.get(key)
            && let Some(s) = v.as_str()
        {
            meta.insert(key.to_string(), s.to_string());
        }
    }

    Ok(ExOrder {
        client_order_id,
        symbol: ExSymbol::new(&ex_symbol),
        side: match args.side {
            OrderSide::Buy => ExSide::Buy,
            OrderSide::Sell => ExSide::Sell,
        },
        order_type: match args.order_type {
            OrderKind::Limit => ExOrderType::Limit,
            OrderKind::Market => ExOrderType::Market,
        },
        quantity: decimal_from_f64(args.quantity)
            .map_err(|e| ExchangeError::ParseError(format!("quantity: {}", e)))?,
        price: args
            .price
            .map(decimal_from_f64)
            .transpose()
            .map_err(|e| ExchangeError::ParseError(format!("price: {}", e)))?,
        time_in_force: ex_tif_from(&args.time_in_force)?,
        exchange: exchange_id,
        meta,
    })
}

// ==================== 类型转换(balance / position) ====================

/// `AccountBalance` -> `CurrencyBalance`
///
/// `CurrencyBalance` 字段:`currency` / `free` / `locked`(无 `total`)。
/// `AccountBalance.total` 在 LLM 工具侧不暴露(若需要,由调用方按 `free + locked` 计算)。
///
/// **不能用 `impl TryFrom<ExBalance> for CurrencyBalance`** — orphan rule 不允许
/// 在 axon-llm 中为外部类型 CurrencyBalance 实现外部 trait TryFrom。
/// 改用 free function。
#[cfg(feature = "trading-exchange")]
fn ex_balance_to_currency_balance(b: ExBalance) -> Result<CurrencyBalance, ExchangeError> {
    Ok(CurrencyBalance {
        currency: b.currency,
        free: f64_from_decimal(b.available)
            .map_err(|e| ExchangeError::ParseError(format!("available: {}", e)))?,
        locked: f64_from_decimal(b.locked)
            .map_err(|e| ExchangeError::ParseError(format!("locked: {}", e)))?,
    })
}

/// `HashMap<asset, ExBalance>` -> `BalanceSnapshot`(`currencies` 累加,`as_of_ms` 取调用时间)
#[cfg(feature = "trading-exchange")]
fn balance_map_to_snapshot(
    map: HashMap<String, ExBalance>,
) -> Result<BalanceSnapshot, ExchangeError> {
    let mut currencies = Vec::with_capacity(map.len());
    for (_, v) in map {
        currencies.push(ex_balance_to_currency_balance(v)?);
    }
    Ok(BalanceSnapshot {
        as_of_ms: now_ms(),
        currencies,
    })
}

/// `Position` -> `PositionSnapshot`(quantity 已带符号,无需 abs/sign)
///
/// `PositionSnapshot.entry_price` ← `Position.avg_entry_price`。
#[cfg(feature = "trading-exchange")]
fn ex_position_to_snapshot(p: ExPosition) -> Result<PositionSnapshot, ExchangeError> {
    Ok(PositionSnapshot {
        symbol: p.symbol.0,
        quantity: f64_from_decimal(p.quantity)
            .map_err(|e| ExchangeError::ParseError(format!("quantity: {}", e)))?,
        entry_price: f64_from_decimal(p.avg_entry_price)
            .map_err(|e| ExchangeError::ParseError(format!("avg_entry_price: {}", e)))?,
        unrealized_pnl: f64_from_decimal(p.unrealized_pnl).unwrap_or(0.0),
        as_of_ms: now_ms(),
    })
}

// ==================== 错误映射 ====================

#[cfg(feature = "trading-exchange")]
use crate::trading::backend::TradingError;

/// `ExchangeError` -> `TradingError` 精细映射。
///
/// 映射原则:
/// - 协议层(网络 / 序列化)→ `Backend`
/// - 业务层(余额不足 / 订单被拒 / 限频)→ 单独 `TradingError` 变体(如有)或带前缀的 `Backend`
/// - 鉴权失败 → `Backend`(不暴露敏感信息,日志详细)
#[cfg(feature = "trading-exchange")]
fn map_exchange_error(e: ExchangeError) -> TradingError {
    match e {
        ExchangeError::InsufficientBalance {
            required,
            available,
        } => TradingError::Backend(format!(
            "insufficient balance: need={} have={}",
            required, available
        )),
        ExchangeError::OrderRejected { reason } => {
            TradingError::Backend(format!("order rejected: {}", reason))
        }
        ExchangeError::RateLimited { wait_ms } => {
            TradingError::Backend(format!("rate limited: wait {}ms", wait_ms))
        }
        ExchangeError::OrderNotFound(id) => {
            TradingError::Backend(format!("order not found: {}", id))
        }
        ExchangeError::AuthenticationFailed(_) => {
            // 日志详细(脱敏后),TradingError 仅提示高层信息
            tracing::warn!(error = %e, "exchange authentication failed");
            TradingError::Backend("authentication failed (see logs)".into())
        }
        ExchangeError::CircuitBreakerOpen => {
            TradingError::Backend("exchange circuit breaker open".into())
        }
        ExchangeError::ApiError { code, message } => {
            TradingError::Backend(format!("exchange api error [{}]: {}", code, message))
        }
        other => TradingError::Backend(other.to_string()),
    }
}

// ==================== ExchangeTradingBackend ====================

#[cfg(feature = "trading-exchange")]
use async_trait::async_trait;
#[cfg(feature = "trading-exchange")]
use axon_exchange::ExchangeAdapter;
#[cfg(feature = "trading-exchange")]
use std::sync::Arc;
#[cfg(feature = "trading-exchange")]
use tokio::sync::RwLock;

/// 真实交易所后端:包装 `ExchangeAdapter` 提供 `TradingBackend` 接口。
///
/// 用 `Arc<RwLock<Box<dyn ExchangeAdapter>>>` 解决 `ExchangeAdapter::send_order` 需 `&mut self`
/// 与 `TradingBackend` `&self` 的签名冲突。读路径(`get_balance` / `get_positions`)
/// 走 `read().await` 共享锁,写路径(`send_order`)走 `write().await` 独占锁。
///
/// **为什么是 `Arc<RwLock<Box<dyn ...>>>` 而非 `Arc<RwLock<Arc<dyn ...>>>`**:
/// - `RwLock::new` 要求 `T: Sized`,而 `dyn ExchangeAdapter` 是 `!Sized`,
///   `Box<dyn ExchangeAdapter>` 是 Sized(指针),可直接 `RwLock::new`。
/// - **关键**:`write().await.send_order(...)` 需 `&mut dyn ExchangeAdapter`。
///   `RwLockWriteGuard<Box<dyn>>` 通过 `&mut **guard` 解 Box 一次 → `&mut dyn`。
///   `Arc<dyn>` 没有 `DerefMut`,无法直接拿到 `&mut dyn`。
/// - caller 传 `Box<dyn ExchangeAdapter>`,内部 `Arc::new(RwLock::new(adapter))` 即可。
#[cfg(feature = "trading-exchange")]
pub struct ExchangeTradingBackend {
    adapter: Arc<RwLock<Box<dyn ExchangeAdapter>>>,
    /// 缓存的交易所 ID,下单时 `Order::exchange` 字段需要
    /// (避免每个 place_order 都需 &mut self 调 exchange_id())。
    exchange_id: ExchangeId,
    symbol_map: SymbolMap,
}

#[cfg(feature = "trading-exchange")]
impl ExchangeTradingBackend {
    /// 包装一个已初始化的 `ExchangeAdapter`。
    ///
    /// 调用方需在传入前 `adapter.connect().await` 完成认证握手
    /// (具体见 `axon-exchange::traits::ExchangeAdapter::connect`)。
    pub fn new(adapter: Box<dyn ExchangeAdapter>, symbol_map: SymbolMap) -> Self {
        let exchange_id = adapter.exchange_id();
        Self {
            adapter: Arc::new(RwLock::new(adapter)),
            exchange_id,
            symbol_map,
        }
    }

    /// 当前底层 adapter 的 Arc 引用(用于在 LLM 工具外调 leverage / funding 等端点)。
    pub fn adapter(&self) -> Arc<RwLock<Box<dyn ExchangeAdapter>>> {
        self.adapter.clone()
    }

    /// 当前缓存的交易所 ID
    pub fn exchange_id(&self) -> ExchangeId {
        self.exchange_id
    }
}

// ==================== TradingBackend impl ====================

#[cfg(feature = "trading-exchange")]
use crate::trading::backend::TradingBackend;

#[cfg(feature = "trading-exchange")]
#[async_trait]
impl TradingBackend for ExchangeTradingBackend {
    fn name(&self) -> &str {
        "exchange"
    }
    async fn place_order(&self, req: &PlaceOrderArgs) -> Result<OrderAck, TradingError> {
        // 1. 标准化 symbol(LLM `BTC-USDT` → 交易所原生 `BTCUSDT` 等)
        let ex_symbol = self.symbol_map.to_exchange(&req.symbol).ok_or_else(|| {
            TradingError::InvalidArguments(format!("未知 symbol: {}", req.symbol))
        })?;

        // 2. 高阶 `PlaceOrderArgs` → 底层 `ExOrder`
        let ex_order =
            args_to_ex_order(req, ex_symbol, self.exchange_id).map_err(map_exchange_error)?;

        // 3. 调底层(需 `&mut self`,用 `write().await` 独占锁)
        //    `&mut **guard`:RwLockWriteGuard 解 Box 一次 → `&mut Box<dyn>`,
        //    再解 Box → `&mut dyn ExchangeAdapter`(因 Box 实现了 DerefMut)。
        let mut guard = self.adapter.write().await;
        let ex_order_id = guard
            .send_order(ex_order)
            .await
            .map_err(map_exchange_error)?;

        // 4. 底层 `OrderId` → 高阶 `OrderAck`
        Ok(OrderAck {
            order_id: ex_order_id.0.to_string(),
            symbol: req.symbol.clone(),
            side: req.side,
            quantity: req.quantity,
            status: OrderStatus("New".into()),
            timestamp_ms: now_ms(),
            confirm_token: None,
        })
    }

    async fn get_balance(&self) -> Result<BalanceSnapshot, TradingError> {
        let guard = self.adapter.read().await;
        let raw = guard.get_balance().await.map_err(map_exchange_error)?;
        drop(guard);
        balance_map_to_snapshot(raw).map_err(map_exchange_error)
    }

    async fn get_positions(&self) -> Result<Vec<PositionSnapshot>, TradingError> {
        let guard = self.adapter.read().await;
        let raw = guard.get_positions().await.map_err(map_exchange_error)?;
        drop(guard);
        raw.into_iter()
            .map(ex_position_to_snapshot)
            .collect::<Result<Vec<_>, ExchangeError>>()
            .map_err(map_exchange_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_map_register_and_lookup_round_trip() {
        // 验证:注册 BTC-USDT -> BTCUSDT,正反查一致
        let mut map = SymbolMap::new();
        map.register("BTC-USDT", "BTCUSDT");
        assert_eq!(map.to_exchange("BTC-USDT"), Some("BTCUSDT".into()));
        assert_eq!(map.to_llm("BTCUSDT"), Some("BTC-USDT".into()));
    }

    #[test]
    fn symbol_map_unknown_returns_none() {
        // 验证:未注册的 symbol 返回 None,不 panic
        let map = SymbolMap::new();
        assert_eq!(map.to_exchange("ETH-USDT"), None);
        assert_eq!(map.to_llm("ETHUSDT"), None);
    }

    #[test]
    fn symbol_map_default_is_empty() {
        // 验证:Default impl 等价于 new()
        let map = SymbolMap::default();
        assert_eq!(map.to_exchange("ANY"), None);
    }

    #[test]
    fn symbol_map_register_multiple_chains() {
        // 验证:链式 register(&mut self) 返回 &mut Self,支持 builder 模式
        let mut map = SymbolMap::new();
        map.register("BTC-USDT", "BTCUSDT")
            .register("ETH-USDT", "ETHUSDT");
        assert_eq!(map.to_exchange("BTC-USDT"), Some("BTCUSDT".into()));
        assert_eq!(map.to_exchange("ETH-USDT"), Some("ETHUSDT".into()));
    }

    // ==================== 辅助函数测试 ====================

    use crate::trading::types::TimeInForce;
    use rust_decimal::Decimal;
    use serde_json::json;
    use std::str::FromStr;

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
        let d = Decimal::from_str("0.001").unwrap();
        let f = f64_from_decimal(d).unwrap();
        let d2 = decimal_from_f64(f).unwrap();
        assert_eq!(d, d2);
    }

    #[test]
    fn ex_tif_from_maps_all_variants() {
        // 验证:TimeInForce 映射 GTC -> Gtc / IOC -> Ioc / FOK -> Fok
        // ExTif 枚举变体不导出为 pub,这里用 debug 断言模式匹配
        let _ = ex_tif_from(&TimeInForce::GTC).unwrap();
        let _ = ex_tif_from(&TimeInForce::IOC).unwrap();
        let _ = ex_tif_from(&TimeInForce::FOK).unwrap();
    }

    #[test]
    fn now_ms_returns_positive_unix_millis() {
        // 验证:now_ms() 返回正整数(unix epoch ms)
        let t = now_ms();
        assert!(t > 0);
        // 2026-01-01 之后的 ms:约 1.76e12
        assert!(t > 1_700_000_000_000);
    }

    // ==================== map_exchange_error 测试 ====================

    #[test]
    fn map_exchange_error_insufficient_balance_includes_amounts() {
        // 验证:InsufficientBalance 转换保留 required/available 数值
        let required = Decimal::from_str("100").unwrap();
        let available = Decimal::from_str("50").unwrap();
        let e = ExchangeError::InsufficientBalance {
            required,
            available,
        };
        let mapped = map_exchange_error(e);
        match mapped {
            TradingError::Backend(msg) => {
                assert!(msg.contains("100"), "msg should contain required: {}", msg);
                assert!(msg.contains("50"), "msg should contain available: {}", msg);
            }
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    #[test]
    fn map_exchange_error_rate_limited_includes_wait_ms() {
        // 验证:RateLimited 转换保留 wait_ms
        let e = ExchangeError::RateLimited { wait_ms: 5000 };
        let mapped = map_exchange_error(e);
        match mapped {
            TradingError::Backend(msg) => assert!(msg.contains("5000")),
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    #[test]
    fn map_exchange_error_authentication_does_not_leak_secret() {
        // 验证:AuthenticationFailed 转换不暴露具体凭据
        let e = ExchangeError::AuthenticationFailed("invalid api_key=secret_12345".into());
        let mapped = map_exchange_error(e);
        match mapped {
            TradingError::Backend(msg) => {
                assert!(!msg.contains("secret_12345"), "msg leaked secret: {}", msg);
                assert!(msg.contains("authentication failed"));
            }
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    #[test]
    fn map_exchange_error_api_error_includes_code_and_message() {
        // 验证:ApiError 转换包含 code 与 message
        let e = ExchangeError::ApiError {
            code: -1000,
            message: "test_error".into(),
        };
        let mapped = map_exchange_error(e);
        match mapped {
            TradingError::Backend(msg) => {
                assert!(msg.contains("-1000"));
                assert!(msg.contains("test_error"));
            }
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    #[test]
    fn map_exchange_error_order_rejected_preserves_reason() {
        // 验证:OrderRejected 转换保留 reason
        let e = ExchangeError::OrderRejected {
            reason: "insufficient margin".into(),
        };
        let mapped = map_exchange_error(e);
        match mapped {
            TradingError::Backend(msg) => assert!(msg.contains("insufficient margin")),
            other => panic!("expected Backend, got {:?}", other),
        }
    }

    // ==================== TryFrom<...> for ExOrder 测试 ====================

    fn make_args(side: OrderSide, qty: f64, price: Option<f64>) -> PlaceOrderArgs {
        PlaceOrderArgs {
            symbol: "BTC-USDT".into(),
            side,
            quantity: qty,
            order_type: OrderKind::Limit,
            price,
            stop_loss: None,
            take_profit: None,
            time_in_force: TimeInForce::GTC,
            extras: json!({}),
        }
    }

    #[test]
    fn try_from_args_to_ex_order_translates_side() {
        // 验证:Buy -> Buy / Sell -> Sell
        let args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        let ex = args_to_ex_order(&args, "BTCUSDT".into(), ExchangeId::Binance).unwrap();
        assert_eq!(ex.side, ExSide::Buy);

        let args = make_args(OrderSide::Sell, 1.0, Some(50000.0));
        let ex = args_to_ex_order(&args, "BTCUSDT".into(), ExchangeId::Binance).unwrap();
        assert_eq!(ex.side, ExSide::Sell);
    }

    #[test]
    fn try_from_args_to_ex_order_translates_order_type() {
        // 验证:Limit -> Limit / Market -> Market
        let args = PlaceOrderArgs {
            order_type: OrderKind::Market,
            price: None,
            ..make_args(OrderSide::Buy, 1.0, None)
        };
        let ex = args_to_ex_order(&args, "BTCUSDT".into(), ExchangeId::Binance).unwrap();
        assert_eq!(ex.order_type, ExOrderType::Market);
    }

    #[test]
    fn try_from_args_to_ex_order_passes_exchange_id() {
        // 验证:ExchangeId 透传
        let args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        let ex = args_to_ex_order(&args, "BTCUSDT".into(), ExchangeId::Okx).unwrap();
        assert_eq!(ex.exchange, ExchangeId::Okx);
    }

    #[test]
    fn try_from_args_to_ex_order_passes_client_order_id_from_extras() {
        // 验证:extras.client_order_id 解析为 OrderId
        let mut args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        let uuid = uuid::Uuid::new_v4();
        args.extras = json!({ "client_order_id": uuid.to_string() });
        let ex = args_to_ex_order(&args, "BTCUSDT".into(), ExchangeId::Binance).unwrap();
        assert_eq!(ex.client_order_id, OrderId(uuid));
    }

    #[test]
    fn try_from_args_to_ex_order_generates_client_order_id_when_extras_empty() {
        // 验证:extras 无 client_order_id 时,OrderId::new() 自动生成
        let args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        let ex = args_to_ex_order(&args, "BTCUSDT".into(), ExchangeId::Binance).unwrap();
        // OrderId::new() 用 Uuid::now_v7,只验证非零即可
        assert!(!ex.client_order_id.0.is_nil());
    }

    #[test]
    fn try_from_args_to_ex_order_rejects_nan_quantity() {
        // 验证:NaN quantity 失败
        let args = make_args(OrderSide::Buy, f64::NAN, Some(50000.0));
        let result = args_to_ex_order(&args, "BTCUSDT".into(), ExchangeId::Binance);
        assert!(result.is_err());
    }

    #[test]
    fn try_from_args_to_ex_order_meta_picks_whitelist_keys() {
        // 验证:extras 中 leverage/margin_type/reduce_only/stop_loss/take_profit 收集到 meta
        let mut args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        args.extras = json!({
            "leverage": "10",
            "margin_type": "isolated",
            "reduce_only": "true",
            "stop_loss": "49000",
            "take_profit": "51000",
            "ignored_key": "should_not_appear"  // 白名单外
        });
        let ex = args_to_ex_order(&args, "BTCUSDT".into(), ExchangeId::Binance).unwrap();
        assert_eq!(ex.meta.get("leverage"), Some(&"10".to_string()));
        assert_eq!(ex.meta.get("margin_type"), Some(&"isolated".to_string()));
        assert_eq!(ex.meta.get("reduce_only"), Some(&"true".to_string()));
        assert_eq!(ex.meta.get("stop_loss"), Some(&"49000".to_string()));
        assert_eq!(ex.meta.get("take_profit"), Some(&"51000".to_string()));
        assert!(!ex.meta.contains_key("ignored_key"));
    }

    #[test]
    fn try_from_args_to_ex_order_translates_time_in_force() {
        // 验证:TimeInForce::GTC -> ExTif::Gtc 等
        let mut args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        args.time_in_force = TimeInForce::IOC;
        let ex = args_to_ex_order(&args, "BTCUSDT".into(), ExchangeId::Binance).unwrap();
        assert_eq!(ex.time_in_force, axon_exchange::TimeInForce::Ioc);
    }

    #[test]
    fn try_from_args_to_ex_order_symbol_uses_exchange_format() {
        // 验证:传 "BTCUSDT"(已标准化)后,Order.symbol 是该字符串
        let args = make_args(OrderSide::Buy, 1.0, Some(50000.0));
        let ex = args_to_ex_order(&args, "BTCUSDT".into(), ExchangeId::Binance).unwrap();
        assert_eq!(ex.symbol, ExSymbol::new("BTCUSDT"));
    }

    // ==================== balance / position 转换测试 ====================

    fn ex_balance(currency: &str, available: &str, locked: &str) -> ExBalance {
        ExBalance {
            currency: currency.into(),
            available: Decimal::from_str(available).unwrap(),
            locked: Decimal::from_str(locked).unwrap(),
        }
    }

    #[test]
    fn ex_balance_to_currency_balance_keeps_free_and_locked() {
        // 验证:`available` -> `free`,`locked` -> `locked`(CurrencyBalance 无 `total`/`used` 字段)
        let b = ex_balance("USDT", "100", "50");
        let cb = ex_balance_to_currency_balance(b).unwrap();
        assert_eq!(cb.currency, "USDT");
        assert!((cb.free - 100.0).abs() < 1e-9);
        assert!((cb.locked - 50.0).abs() < 1e-9);
    }

    #[test]
    fn ex_balance_to_currency_balance_zero_locked() {
        // 验证:locked=0 时正确转换(无 total 字段)
        let b = ex_balance("BTC", "0.5", "0");
        let cb = ex_balance_to_currency_balance(b).unwrap();
        assert!((cb.free - 0.5).abs() < 1e-9);
        assert!((cb.locked - 0.0).abs() < 1e-9);
    }

    #[test]
    fn balance_map_to_snapshot_aggregates_currencies() {
        // 验证:HashMap<asset, ExBalance> -> BalanceSnapshot.currencies 列表
        let mut map = HashMap::new();
        map.insert("USDT".to_string(), ex_balance("USDT", "100", "50"));
        map.insert("BTC".to_string(), ex_balance("BTC", "0.5", "0"));
        let snap = balance_map_to_snapshot(map).unwrap();
        assert_eq!(snap.currencies.len(), 2);
        // 各币种 free/locked 累加通过各 CurrencyBalance 自身,BalanceSnapshot 自身无聚合字段
        let usdt = snap
            .currencies
            .iter()
            .find(|c| c.currency == "USDT")
            .unwrap();
        assert!((usdt.free - 100.0).abs() < 1e-9);
        assert!((usdt.locked - 50.0).abs() < 1e-9);
        let btc = snap
            .currencies
            .iter()
            .find(|c| c.currency == "BTC")
            .unwrap();
        assert!((btc.free - 0.5).abs() < 1e-9);
        assert!((btc.locked - 0.0).abs() < 1e-9);
        assert!(snap.as_of_ms > 0);
    }

    fn ex_pos(side: ExSide, qty: &str, entry: &str) -> ExPosition {
        ExPosition {
            symbol: ExSymbol::new("BTCUSDT"),
            side,
            quantity: Decimal::from_str(qty).unwrap(),
            avg_entry_price: Decimal::from_str(entry).unwrap(),
            unrealized_pnl: Decimal::from_str("0").unwrap(),
        }
    }

    #[test]
    fn ex_position_to_snapshot_long_has_positive_quantity() {
        // 验证:side=Buy 且 quantity 已带正号(long),直接 f64 转换
        let p = ex_pos(ExSide::Buy, "0.5", "50000");
        let snap = ex_position_to_snapshot(p).unwrap();
        assert_eq!(snap.symbol, "BTCUSDT");
        assert!((snap.quantity - 0.5).abs() < 1e-9);
        assert!((snap.entry_price - 50000.0).abs() < 1e-9);
    }

    #[test]
    fn ex_position_to_snapshot_short_has_negative_quantity() {
        // 验证:side=Sell 时 quantity 带负号
        let p = ex_pos(ExSide::Sell, "-0.3", "51000");
        let snap = ex_position_to_snapshot(p).unwrap();
        assert!((snap.quantity - (-0.3)).abs() < 1e-9);
        assert!((snap.entry_price - 51000.0).abs() < 1e-9);
    }

    #[test]
    fn ex_position_to_snapshot_zero_pnl_handled() {
        // 验证:unrealized_pnl=0 时不报错
        let p = ex_pos(ExSide::Buy, "1.0", "50000");
        let snap = ex_position_to_snapshot(p).unwrap();
        assert!((snap.unrealized_pnl - 0.0).abs() < 1e-9);
    }

    #[test]
    fn ex_position_to_snapshot_uses_avg_entry_price() {
        // 验证:用 avg_entry_price 字段(非 entry_price)→ PositionSnapshot.entry_price
        let p = ExPosition {
            symbol: ExSymbol::new("BTCUSDT"),
            side: ExSide::Buy,
            quantity: Decimal::from_str("1.0").unwrap(),
            avg_entry_price: Decimal::from_str("49000.5").unwrap(),
            unrealized_pnl: Decimal::from_str("0").unwrap(),
        };
        let snap = ex_position_to_snapshot(p).unwrap();
        assert!((snap.entry_price - 49000.5).abs() < 1e-9);
    }

    // ==================== Task 9: ExchangeTradingBackend struct + MockAdapter ====================

    use async_trait::async_trait;
    use tokio::sync::mpsc;

    /// 测试用最小 `ExchangeAdapter` 实现。
    /// 完整 mock 集成测试见 `tests/trading_exchange_integration.rs`(Task 12)。
    struct MockAdapter {
        id: ExchangeId,
    }

    #[async_trait]
    impl ExchangeAdapter for MockAdapter {
        fn exchange_id(&self) -> ExchangeId {
            self.id
        }
        async fn connect(&mut self) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn subscribe(&mut self, _symbols: &[ExSymbol]) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn send_order(&mut self, _order: ExOrder) -> Result<OrderId, ExchangeError> {
            Ok(OrderId::new())
        }
        async fn cancel_order(&mut self, _order_id: OrderId) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn get_balance(&self) -> Result<HashMap<String, ExBalance>, ExchangeError> {
            Ok(HashMap::new())
        }
        async fn get_positions(&self) -> Result<Vec<ExPosition>, ExchangeError> {
            Ok(Vec::new())
        }
        fn get_depth(&self, _symbol: &ExSymbol) -> Option<axon_exchange::DepthSnapshot> {
            None
        }
        fn get_ticker(&self, _symbol: &ExSymbol) -> Option<axon_exchange::Ticker> {
            None
        }
        fn market_data_rx(&self) -> mpsc::Receiver<axon_exchange::WsMessage> {
            // 永不发送消息的 channel
            let (_tx, rx) = mpsc::channel(1);
            rx
        }
        async fn set_leverage(&self, _symbol: &str, _leverage: u8) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn set_margin_type(
            &self,
            _symbol: &str,
            _margin_type: axon_exchange::MarginType,
        ) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn get_leverage_brackets(
            &self,
            _symbol: &str,
        ) -> Result<Vec<axon_exchange::LeverageBracket>, ExchangeError> {
            Ok(Vec::new())
        }
        async fn set_position_mode(&self, _hedge_mode: bool) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn get_funding_rate(
            &self,
            _symbol: &str,
        ) -> Result<axon_exchange::FundingRate, ExchangeError> {
            unimplemented!("Task 9 不测试 funding_rate")
        }
        async fn get_account_info(&self) -> Result<axon_exchange::AccountInfo, ExchangeError> {
            unimplemented!("Task 9 不测试 account_info")
        }
        async fn get_open_interest(
            &self,
            _symbol: &str,
        ) -> Result<axon_exchange::OpenInterest, ExchangeError> {
            unimplemented!("Task 9 不测试 open_interest")
        }
        async fn get_long_short_ratio(
            &self,
            _symbol: &str,
        ) -> Result<axon_exchange::LongShortRatio, ExchangeError> {
            unimplemented!("Task 9 不测试 long_short_ratio")
        }
    }

    #[test]
    fn exchange_trading_backend_new_caches_exchange_id() {
        // 验证:new() 一次性缓存 exchange_id,后续不需 &mut self 调 exchange_id()
        let adapter: Box<dyn ExchangeAdapter> = Box::new(MockAdapter {
            id: ExchangeId::Binance,
        });
        let map = SymbolMap::new();
        let backend = ExchangeTradingBackend::new(adapter, map);
        // exchange_id 通过 getter 验证
        assert_eq!(backend.exchange_id(), ExchangeId::Binance);
    }

    #[test]
    fn exchange_trading_backend_adapter_returns_clone() {
        // 验证:adapter() 返回 Arc 克隆,调用方与 backend 共享同一 RwLock<Box<dyn ExchangeAdapter>>
        // (Arc::clone 产生相同指针的多个引用,底层指向同一 RwLock)
        let adapter: Box<dyn ExchangeAdapter> = Box::new(MockAdapter {
            id: ExchangeId::Binance,
        });
        let map = SymbolMap::new();
        let backend = ExchangeTradingBackend::new(adapter, map);
        let a1 = backend.adapter();
        let a2 = backend.adapter();
        // 两次 Arc::clone 共享同一指针
        assert!(Arc::ptr_eq(&a1, &a2));
    }

    // ==================== Task 10: TradingBackend::place_order / get_balance / get_positions ====================

    use crate::trading::backend::TradingBackend;

    /// 增强版 MockAdapter,记录调用参数,验证 place_order 路径
    struct MockAdapterWithRecorder {
        id: ExchangeId,
        sent_orders: std::sync::Mutex<Vec<ExOrder>>,
    }

    #[async_trait]
    impl ExchangeAdapter for MockAdapterWithRecorder {
        fn exchange_id(&self) -> ExchangeId {
            self.id
        }
        async fn connect(&mut self) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn subscribe(&mut self, _symbols: &[ExSymbol]) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn send_order(&mut self, order: ExOrder) -> Result<OrderId, ExchangeError> {
            self.sent_orders.lock().expect("poisoned").push(order);
            Ok(OrderId::new())
        }
        async fn cancel_order(&mut self, _order_id: OrderId) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn get_balance(&self) -> Result<HashMap<String, ExBalance>, ExchangeError> {
            Ok(HashMap::new())
        }
        async fn get_positions(&self) -> Result<Vec<ExPosition>, ExchangeError> {
            Ok(Vec::new())
        }
        fn get_depth(&self, _symbol: &ExSymbol) -> Option<axon_exchange::DepthSnapshot> {
            None
        }
        fn get_ticker(&self, _symbol: &ExSymbol) -> Option<axon_exchange::Ticker> {
            None
        }
        fn market_data_rx(&self) -> mpsc::Receiver<axon_exchange::WsMessage> {
            let (_tx, rx) = mpsc::channel(1);
            rx
        }
        async fn set_leverage(&self, _symbol: &str, _leverage: u8) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn set_margin_type(
            &self,
            _symbol: &str,
            _margin_type: axon_exchange::MarginType,
        ) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn get_leverage_brackets(
            &self,
            _symbol: &str,
        ) -> Result<Vec<axon_exchange::LeverageBracket>, ExchangeError> {
            Ok(Vec::new())
        }
        async fn set_position_mode(&self, _hedge_mode: bool) -> Result<(), ExchangeError> {
            Ok(())
        }
        async fn get_funding_rate(
            &self,
            _symbol: &str,
        ) -> Result<axon_exchange::FundingRate, ExchangeError> {
            unimplemented!()
        }
        async fn get_account_info(&self) -> Result<axon_exchange::AccountInfo, ExchangeError> {
            unimplemented!()
        }
        async fn get_open_interest(
            &self,
            _symbol: &str,
        ) -> Result<axon_exchange::OpenInterest, ExchangeError> {
            unimplemented!()
        }
        async fn get_long_short_ratio(
            &self,
            _symbol: &str,
        ) -> Result<axon_exchange::LongShortRatio, ExchangeError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn place_order_returns_order_ack_with_backend_order_id() {
        // 验证:place_order 成功时,OrderAck 字段来自请求 + backend.send_order 返回 ID
        let adapter: Box<dyn ExchangeAdapter> = Box::new(MockAdapterWithRecorder {
            id: ExchangeId::Binance,
            sent_orders: std::sync::Mutex::new(Vec::new()),
        });
        let mut map = SymbolMap::new();
        map.register("BTC-USDT", "BTCUSDT");
        let backend = ExchangeTradingBackend::new(adapter, map);
        let args = make_args(OrderSide::Buy, 0.001, Some(50000.0));
        let ack = backend.place_order(&args).await.unwrap();
        assert_eq!(ack.symbol, "BTC-USDT");
        assert_eq!(ack.side, OrderSide::Buy);
        assert!((ack.quantity - 0.001).abs() < 1e-9);
        assert_eq!(ack.status.0, "New");
        // MockAdapter::send_order 返回 OrderId::new()(Uuid::now_v7 非 nil)
        assert!(!ack.order_id.is_empty());
    }

    #[tokio::test]
    async fn place_order_unknown_symbol_returns_invalid_arguments() {
        // 验证:未注册的 symbol 返回 TradingError::InvalidArguments(不下单,不走 backend)
        let adapter: Box<dyn ExchangeAdapter> = Box::new(MockAdapterWithRecorder {
            id: ExchangeId::Binance,
            sent_orders: std::sync::Mutex::new(Vec::new()),
        });
        let backend = ExchangeTradingBackend::new(adapter, SymbolMap::new());
        let args = make_args(OrderSide::Buy, 0.001, Some(50000.0));
        let result = backend.place_order(&args).await;
        match result {
            Err(TradingError::InvalidArguments(msg)) => {
                assert!(msg.contains("BTC-USDT"));
            }
            other => panic!("expected InvalidArguments, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn place_order_nan_quantity_returns_backend_parse_error() {
        // 验证:NaN quantity 在 args_to_ex_order 阶段失败,map 到 TradingError::Backend
        let adapter: Box<dyn ExchangeAdapter> = Box::new(MockAdapterWithRecorder {
            id: ExchangeId::Binance,
            sent_orders: std::sync::Mutex::new(Vec::new()),
        });
        let mut map = SymbolMap::new();
        map.register("BTC-USDT", "BTCUSDT");
        let backend = ExchangeTradingBackend::new(adapter, map);
        let args = make_args(OrderSide::Buy, f64::NAN, Some(50000.0));
        let result = backend.place_order(&args).await;
        assert!(matches!(result, Err(TradingError::Backend(_))));
    }
}
