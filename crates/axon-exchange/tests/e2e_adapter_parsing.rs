//! 端到端测试:axon-exchange 适配器配置 + 订单生命周期 + 限流器
//!
//! ## 5 个测试场景
//!
//! 1. `config_roundtrip_serialization`:ExchangeConfig JSON 序列化 → 反序列化 → 字段一致
//! 2. `order_lifecycle_register_to_filled`:OrderLifecycleManager 注册 → 状态更新 → 终态归档
//! 3. `order_lifecycle_cancel_path`:注册 → 取消 → 终态归档
//! 4. `rate_limiter_capacity_and_refill`:限流器容量 + 时间补充
//! 5. `ws_manager_state_transitions`:WebSocketManager 连接成功/失败/熔断状态转换
//!
//! 运行:`cargo test -p axon-exchange --test e2e_adapter_parsing`

use axon_exchange::types::{Order, OrderId, OrderStatus, OrderType, Side, TimeInForce};
use axon_exchange::{
    ExchangeConfig, ExchangeId, OrderLifecycleManager, RateLimitConfig, ReconnectConfig, Symbol,
    TokenBucketRateLimiter, WebSocketManager,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::time::Duration;

// ── helpers ────────────────────────────────────────────────────────────

fn testnet_config() -> ExchangeConfig {
    ExchangeConfig {
        exchange_id: ExchangeId::Binance,
        api_key: "test_key".into(),
        api_secret: "test_secret".into(),
        passphrase: None,
        testnet: true,
        rest_base_url: "https://testnet.binance.vision".into(),
        ws_url: "wss://testnet.binance.vision/ws".into(),
        rate_limit: RateLimitConfig {
            requests_per_second: 10,
            orders_per_minute: 60,
            ws_messages_per_second: 50,
        },
        reconnect: ReconnectConfig {
            max_retries: 10,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            circuit_breaker_threshold: 5,
            circuit_breaker_reset: Duration::from_secs(60),
        },
        proxy: None,
        position_endpoint: "/fapi/v2/positionRisk".into(),
        fapi_base_url: None,
    }
}

fn test_order() -> Order {
    Order {
        client_order_id: OrderId::new(),
        symbol: Symbol::new("BTCUSDT"),
        side: Side::Buy,
        order_type: OrderType::Limit,
        price: Some(Decimal::from(50000)),
        quantity: Decimal::from(1),
        time_in_force: TimeInForce::Gtc,
        exchange: ExchangeId::Binance,
        meta: HashMap::new(),
    }
}

// ── 1. Config JSON 序列化 → 反序列化 → 字段一致 ──────────────────────

#[test]
fn config_roundtrip_serialization() {
    let config = testnet_config();
    let json = serde_json::to_string(&config).unwrap();
    let restored: ExchangeConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.exchange_id, ExchangeId::Binance);
    assert_eq!(restored.api_key, "test_key");
    assert!(restored.testnet);
    assert_eq!(
        restored.rate_limit.requests_per_second,
        config.rate_limit.requests_per_second
    );
    assert_eq!(restored.reconnect.max_retries, config.reconnect.max_retries);
}

// ── 2. OrderLifecycleManager: 注册 → 状态更新 → 终态归档 ─────────────

#[test]
fn order_lifecycle_register_to_filled() {
    let manager = OrderLifecycleManager::new();
    let order = test_order();
    let id = manager.register_order(order);

    // 注册后 active_count = 1
    assert_eq!(manager.active_count(), 1);

    // 更新到 Acknowledged
    manager
        .update_status(id, OrderStatus::Acknowledged)
        .unwrap();
    assert_eq!(manager.active_count(), 1);

    // 更新到 Filled(终态) → 应从 active 移入 history
    manager
        .update_status(
            id,
            OrderStatus::Filled {
                filled_qty: Decimal::from(1),
                avg_price: Decimal::from(50000),
            },
        )
        .unwrap();

    // active 中应已移除, history 中应有记录
    assert_eq!(manager.active_count(), 0);
    assert_eq!(manager.history_count(), 1);
}

// ── 3. OrderLifecycleManager: 注册 → 取消 → 终态归档 ─────────────────

#[test]
fn order_lifecycle_cancel_path() {
    let manager = OrderLifecycleManager::new();
    let order = test_order();
    let id = manager.register_order(order);

    manager
        .update_status(id, OrderStatus::Acknowledged)
        .unwrap();
    manager
        .update_status(
            id,
            OrderStatus::Cancelled {
                filled_qty: Decimal::ZERO,
            },
        )
        .unwrap();

    assert_eq!(manager.active_count(), 0);
    assert_eq!(manager.history_count(), 1);
}

// ── 4. RateLimiter: 容量 + 时间补充 ────────────────────────────────────

#[test]
fn rate_limiter_capacity_and_refill() {
    let mut limiter = TokenBucketRateLimiter::new(5);
    assert_eq!(limiter.capacity(), 5);
    assert_eq!(limiter.refill_rate(), 5.0);

    // 消耗全部 token
    for _ in 0..5 {
        assert!(limiter.try_acquire().is_ok());
    }
    // 第 6 次应被拒绝
    assert!(limiter.try_acquire().is_err());

    // 等待 1 秒后应补充 5 个 token
    std::thread::sleep(Duration::from_secs(1));
    assert!(limiter.try_acquire().is_ok());
}

// ── 5. WebSocketManager: 连接成功/失败/熔断状态转换 ────────────────────

#[test]
fn ws_manager_state_transitions() {
    let (tx, _rx) = tokio::sync::mpsc::channel(10);
    let config = ReconnectConfig {
        max_retries: 10,
        initial_backoff: Duration::from_millis(500),
        max_backoff: Duration::from_secs(30),
        backoff_multiplier: 2.0,
        circuit_breaker_threshold: 3,
        circuit_breaker_reset: Duration::from_secs(60),
    };
    let (manager, _watch_rx) = WebSocketManager::new(config, tx);

    // 初始状态
    assert!(!manager.is_connected());
    assert!(!manager.is_circuit_open());

    // 连接成功
    manager.on_connect_success();
    assert!(manager.is_connected());
    assert!(!manager.is_circuit_open());

    // 连接失败 1 次
    manager.on_connect_failure();
    assert!(!manager.is_connected());
    assert!(!manager.is_circuit_open()); // 未达阈值

    // 连接失败 2 次
    manager.on_connect_failure();
    assert!(!manager.is_circuit_open());

    // 连接失败 3 次 → 熔断
    manager.on_connect_failure();
    assert!(manager.is_circuit_open());

    // 重新连接成功 → 熔断重置
    manager.on_connect_success();
    assert!(manager.is_connected());
    assert!(!manager.is_circuit_open());
}
