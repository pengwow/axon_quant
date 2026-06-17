//! wiremock 集成测试:ExchangeTradingBackend 跨 crate 引用 + HTTP 路径
//!
//! 目的:验证 ExchangeTradingBackend 作为 `pub` API 可被 crate 外访问,以及
//! 它能把 TradingBackend trait 调用正确桥接到 ExchangeAdapter trait。
//!
//! 注:本测试**不依赖真实 BinanceAdapter**(其 connect 启动 WS,wiremock 不支持)。
//! 改为使用轻量 MockExchangeAdapter,聚焦验证 ExchangeTradingBackend 包装层。
//! 真实 HTTP 路径的 E2E 测试见 `trading_exchange_testnet.rs`(testnet @ignore)。

#![cfg(feature = "trading-exchange")]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axon_exchange::{
    AccountBalance, ExchangeAdapter, ExchangeError, ExchangeId, Order, OrderId, Position, Symbol,
};
use axon_llm::trading::{
    ExchangeTradingBackend, OrderKind, OrderSide, PlaceOrderArgs, SymbolMap, TimeInForce,
    TradingBackend,
};
use rust_decimal::Decimal;
use serde_json::json;
use tokio::sync::{Mutex, mpsc};

/// Mock 内部状态:所有可变字段集中放在此,经 Arc<Mutex<...>> 共享。
///
/// 这样设计的原因:
/// - `ExchangeAdapter::send_order` 等 `&mut self` 方法需要修改状态,但测试
///   又想跨调用读取状态(`sent_orders`)用于断言。
/// - 通过把状态放在 `Arc<Mutex<...>>` 内,adapter 本身只持 `Arc` 共享引用,
///   测试也持同一 Arc 克隆用于断言。
#[derive(Default)]
struct MockState {
    sent_orders: Vec<Order>,
    balance_response: HashMap<String, AccountBalance>,
}

/// Mock ExchangeAdapter(简化版,只实现测试需要的最小集)
///
/// 注:lib 测试中也有 MockAdapter,这里独立定义以避免 pub-test 标记泄露。
struct MockExchangeAdapter {
    id: ExchangeId,
    state: Arc<Mutex<MockState>>,
}

impl MockExchangeAdapter {
    fn new(id: ExchangeId) -> (Self, Arc<Mutex<MockState>>) {
        let state = Arc::new(Mutex::new(MockState::default()));
        let adapter = Self {
            id,
            state: state.clone(),
        };
        (adapter, state)
    }
}

#[async_trait]
impl ExchangeAdapter for MockExchangeAdapter {
    fn exchange_id(&self) -> ExchangeId {
        self.id
    }
    async fn connect(&mut self) -> Result<(), ExchangeError> {
        Ok(())
    }
    async fn disconnect(&mut self) -> Result<(), ExchangeError> {
        Ok(())
    }
    async fn subscribe(&mut self, _symbols: &[Symbol]) -> Result<(), ExchangeError> {
        Ok(())
    }
    async fn send_order(&mut self, order: Order) -> Result<OrderId, ExchangeError> {
        let mut state = self.state.lock().await;
        state.sent_orders.push(order.clone());
        Ok(order.client_order_id)
    }
    async fn cancel_order(&mut self, _order_id: OrderId) -> Result<(), ExchangeError> {
        Ok(())
    }
    async fn get_balance(&self) -> Result<HashMap<String, AccountBalance>, ExchangeError> {
        let state = self.state.lock().await;
        Ok(state.balance_response.clone())
    }
    async fn get_positions(&self) -> Result<Vec<Position>, ExchangeError> {
        Ok(Vec::new())
    }
    fn get_depth(&self, _symbol: &Symbol) -> Option<axon_exchange::DepthSnapshot> {
        None
    }
    fn get_ticker(&self, _symbol: &Symbol) -> Option<axon_exchange::Ticker> {
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
        Err(ExchangeError::ParseError("not supported".into()))
    }
    async fn get_account_info(&self) -> Result<axon_exchange::AccountInfo, ExchangeError> {
        Err(ExchangeError::ParseError("not supported".into()))
    }
    async fn get_open_interest(
        &self,
        _symbol: &str,
    ) -> Result<axon_exchange::OpenInterest, ExchangeError> {
        Err(ExchangeError::ParseError("not supported".into()))
    }
    async fn get_long_short_ratio(
        &self,
        _symbol: &str,
    ) -> Result<axon_exchange::LongShortRatio, ExchangeError> {
        Err(ExchangeError::ParseError("not supported".into()))
    }
}

fn make_buy_args(symbol: &str, qty: f64, price: f64) -> PlaceOrderArgs {
    PlaceOrderArgs {
        symbol: symbol.into(),
        side: OrderSide::Buy,
        quantity: qty,
        order_type: OrderKind::Limit,
        price: Some(price),
        stop_loss: None,
        take_profit: None,
        time_in_force: TimeInForce::GTC,
        extras: json!({}),
    }
}

#[tokio::test]
async fn place_order_translates_high_level_to_low_level_order() {
    // 验证:ExchangeTradingBackend.place_order 正确桥接 LLM 工具语义到 ExchangeAdapter
    let (adapter, state) = MockExchangeAdapter::new(ExchangeId::Binance);
    let mut map = SymbolMap::new();
    map.register("BTC-USDT", "BTCUSDT");
    let backend = ExchangeTradingBackend::new(Box::new(adapter), map);
    let args = make_buy_args("BTC-USDT", 0.001, 50000.0);
    let ack = backend.place_order(&args).await.expect("place_order");
    // OrderAck 字段透传
    assert_eq!(ack.symbol, "BTC-USDT");
    assert_eq!(ack.side, OrderSide::Buy);
    assert!((ack.quantity - 0.001).abs() < 1e-9);
    assert!(!ack.order_id.is_empty());

    // 验证:MockAdapter 收到 1 个订单,symbol 已标准化为 "BTCUSDT"
    let state = state.lock().await;
    assert_eq!(state.sent_orders.len(), 1);
    assert_eq!(state.sent_orders[0].symbol.0, "BTCUSDT");
}

#[tokio::test]
async fn get_balance_parses_account_balance_to_currency_balance() {
    // 验证:HashMap<asset, AccountBalance> → BalanceSnapshot 正确转换
    let (adapter, state) = MockExchangeAdapter::new(ExchangeId::Binance);
    {
        let mut state = state.lock().await;
        state.balance_response.insert(
            "USDT".to_string(),
            AccountBalance {
                currency: "USDT".into(),
                available: Decimal::new(10050, 2), // 100.50
                locked: Decimal::new(5000, 2),     // 50.00
            },
        );
    }
    let backend = ExchangeTradingBackend::new(Box::new(adapter), SymbolMap::new());

    let snap = backend.get_balance().await.expect("get_balance");
    assert_eq!(snap.currencies.len(), 1);
    let usdt = &snap.currencies[0];
    assert_eq!(usdt.currency, "USDT");
    assert!((usdt.free - 100.50).abs() < 1e-9);
    assert!((usdt.locked - 50.00).abs() < 1e-9);
    assert!(snap.as_of_ms > 0);
}

#[tokio::test]
async fn get_balance_empty_account_returns_zero_currencies() {
    // 验证:空账户 → BalanceSnapshot.currencies 为空
    let (adapter, _state) = MockExchangeAdapter::new(ExchangeId::Binance);
    let backend = ExchangeTradingBackend::new(Box::new(adapter), SymbolMap::new());

    let snap = backend.get_balance().await.expect("get_balance");
    assert!(snap.currencies.is_empty());
    assert!(snap.as_of_ms > 0);
}

#[tokio::test]
async fn place_order_unknown_symbol_returns_invalid_arguments() {
    // 验证:未注册的 symbol 返回 TradingError::InvalidArguments(不下单)
    let (adapter, state) = MockExchangeAdapter::new(ExchangeId::Binance);
    let backend = ExchangeTradingBackend::new(Box::new(adapter), SymbolMap::new());
    let args = make_buy_args("UNKNOWN", 0.001, 50000.0);
    let result = backend.place_order(&args).await;
    assert!(matches!(
        result,
        Err(axon_llm::trading::TradingError::InvalidArguments(_))
    ));
    // 验证:adapter 收到 0 个订单
    let state = state.lock().await;
    assert_eq!(state.sent_orders.len(), 0);
}

#[tokio::test]
async fn place_order_meta_picks_whitelist_keys() {
    // 验证:extras 白名单 (leverage/margin_type/reduce_only/stop_loss/take_profit) 透传到 Order.meta
    let (adapter, state) = MockExchangeAdapter::new(ExchangeId::Binance);
    let mut map = SymbolMap::new();
    map.register("BTC-USDT", "BTCUSDT");
    let backend = ExchangeTradingBackend::new(Box::new(adapter), map);
    let mut args = make_buy_args("BTC-USDT", 0.001, 50000.0);
    args.extras = json!({
        "leverage": "10",
        "margin_type": "isolated",
        "reduce_only": "true",
        "stop_loss": "49000",
        "take_profit": "51000",
        "ignored_key": "should_not_appear"
    });
    backend.place_order(&args).await.expect("place_order");
    let state = state.lock().await;
    assert_eq!(state.sent_orders.len(), 1);
    let meta = &state.sent_orders[0].meta;
    assert_eq!(meta.get("leverage"), Some(&"10".to_string()));
    assert_eq!(meta.get("margin_type"), Some(&"isolated".to_string()));
    assert_eq!(meta.get("reduce_only"), Some(&"true".to_string()));
    assert_eq!(meta.get("stop_loss"), Some(&"49000".to_string()));
    assert_eq!(meta.get("take_profit"), Some(&"51000".to_string()));
    assert!(!meta.contains_key("ignored_key"));
}

#[tokio::test]
async fn get_positions_handles_empty_response() {
    // 验证:get_positions 空列表 → Vec<PositionSnapshot> 为空
    let (adapter, _state) = MockExchangeAdapter::new(ExchangeId::Binance);
    let backend = ExchangeTradingBackend::new(Box::new(adapter), SymbolMap::new());

    let positions = backend.get_positions().await.expect("get_positions");
    assert!(positions.is_empty());
}

#[tokio::test]
async fn adapter_accessor_returns_shared_lock() {
    // 验证:adapter() 返回的 Arc<RwLock<...>> 多次调用共享同一 RwLock
    let (adapter, _state) = MockExchangeAdapter::new(ExchangeId::Binance);
    let backend = ExchangeTradingBackend::new(Box::new(adapter), SymbolMap::new());
    let a1 = backend.adapter();
    let a2 = backend.adapter();
    assert!(Arc::ptr_eq(&a1, &a2));
}
