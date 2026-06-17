//! 真实 Binance testnet 端到端测试
//!
//! 默认 `@ignore`,启用需设置环境变量:
//! - `AXON_RUN_BINANCE_TESTNET=1` 启用
//! - `AXON_BINANCE_TESTNET_API_KEY` / `AXON_BINANCE_TESTNET_API_SECRET` testnet 凭证
//!
//! 验证:`ExchangeTradingBackend` 走完整 HTTP 路径,在真实 testnet 下:
//! 1. `connect()` 鉴权握手
//! 2. `get_balance()` 查询 USDT 余额
//! 3. `place_order()` 下 0.001 BTC-USDT Limit 远低于市价(自然过期)
//! 4. 返回 `OrderAck.order_id` 非空
//!
//! **不撤单**(需 Stage E `cancel_order` 扩展 `TradingBackend`)。

#![cfg(feature = "trading-exchange")]

use std::env;
use std::time::Duration;

use axon_exchange::adapters::binance::BinanceAdapter;
use axon_exchange::{
    ExchangeAdapter, ExchangeConfig, ExchangeId, RateLimitConfig, ReconnectConfig,
};
use axon_llm::trading::{
    ExchangeTradingBackend, OrderKind, OrderSide, PlaceOrderArgs, SymbolMap, TimeInForce,
    TradingBackend,
};
use serde_json::json;

/// 读取 testnet 凭证 + REST base URL(env 缺失返回 None,触发测试跳过)。
fn testnet_credentials() -> Option<(String, String, String)> {
    let key = env::var("AXON_BINANCE_TESTNET_API_KEY").ok()?;
    let secret = env::var("AXON_BINANCE_TESTNET_API_SECRET").ok()?;
    let base = env::var("AXON_BINANCE_TESTNET_REST_URL")
        .unwrap_or_else(|_| "https://testnet.binance.vision".into());
    Some((base, key, secret))
}

/// 构造测试用 `ExchangeConfig`(testnet + 默认限流/重连)。
fn build_testnet_config(base_url: String, key: String, secret: String) -> ExchangeConfig {
    ExchangeConfig {
        exchange_id: ExchangeId::Binance,
        api_key: key,
        api_secret: secret,
        passphrase: None,
        testnet: true,
        rest_base_url: base_url,
        ws_url: "wss://testnet.binance.vision/ws".into(),
        rate_limit: RateLimitConfig {
            // testnet 配额宽松,使用生产一致的保守值
            requests_per_second: 10,
            orders_per_minute: 60,
            ws_messages_per_second: 50,
        },
        reconnect: ReconnectConfig {
            max_retries: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
            backoff_multiplier: 2.0,
            circuit_breaker_threshold: 5,
            circuit_breaker_reset: Duration::from_secs(60),
        },
        proxy: None,
        position_endpoint: String::new(), // 现货不查持仓
        fapi_base_url: None,
    }
}

#[tokio::test]
#[ignore = "requires AXON_RUN_BINANCE_TESTNET=1 and testnet credentials"]
async fn place_then_verify_testnet_binance() {
    // 1. 检查启用开关(双重保险:`#[ignore]` 已默认跳过)
    if env::var("AXON_RUN_BINANCE_TESTNET").ok().as_deref() != Some("1") {
        eprintln!("set AXON_RUN_BINANCE_TESTNET=1 to run this test");
        return;
    }

    // 2. 读取 testnet 凭证
    let (base, key, secret) = testnet_credentials().expect("missing testnet creds");
    let config = build_testnet_config(base, key, secret);

    // 3. 构造 adapter + 鉴权
    let mut adapter = BinanceAdapter::new(config);
    adapter.connect().await.expect("connect testnet failed");

    // 4. 包成 ExchangeTradingBackend
    let mut map = SymbolMap::new();
    map.register("BTC-USDT", "BTCUSDT");
    let backend = ExchangeTradingBackend::new(Box::new(adapter), map);

    // 5. 查初始余额,验证 USDT 存在(testnet 注册会送)
    let initial = backend.get_balance().await.expect("get_balance");
    println!("initial balances: {} currencies", initial.currencies.len());
    for c in &initial.currencies {
        println!("  {}: free={} locked={}", c.currency, c.free, c.locked);
    }
    assert!(
        initial.currencies.iter().any(|c| c.currency == "USDT"),
        "testnet account should have USDT"
    );

    // 6. 下单(Limit 远低于市价,testnet 不会成交 → 自然过期)
    let args = PlaceOrderArgs {
        symbol: "BTC-USDT".into(),
        side: OrderSide::Buy,
        quantity: 0.001,
        order_type: OrderKind::Limit,
        // 10000 USDT 远低于 BTC 当前价格(>50000),挂单不会成交
        price: Some(10_000.0),
        stop_loss: None,
        take_profit: None,
        time_in_force: TimeInForce::GTC,
        extras: json!({}),
    };
    let ack = backend.place_order(&args).await.expect("place_order");
    assert!(
        !ack.order_id.is_empty(),
        "OrderAck.order_id must be non-empty"
    );
    assert_eq!(ack.symbol, "BTC-USDT");
    assert_eq!(ack.side, OrderSide::Buy);
    assert!((ack.quantity - 0.001).abs() < 1e-9);
    println!("order placed: {} (status: {})", ack.order_id, ack.status.0);

    // 7. 不撤单,Limit 远低于市价会自然过期
    //    撤单需 Stage E 在 TradingBackend 扩展 cancel_order
    println!("E2E test complete: order {} left to expire", ack.order_id);
}
