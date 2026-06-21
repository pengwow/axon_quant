//! axon-exchange 端到端测试

use axon_exchange::{ExchangeError, ExchangeId, RateLimitConfig, ReconnectConfig};
use std::time::Duration;

// ═══════════════════════════════════════════════════════════════════════════
// ExchangeId 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_exchange_id_variants() {
    assert_eq!(ExchangeId::Binance, ExchangeId::Binance);
    assert_eq!(ExchangeId::Okx, ExchangeId::Okx);
    assert_ne!(ExchangeId::Binance, ExchangeId::Okx);
}

#[test]
fn test_exchange_id_serialization() {
    let ids = vec![ExchangeId::Binance, ExchangeId::Okx];
    for id in ids {
        let json = serde_json::to_string(&id).unwrap();
        let restored: ExchangeId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// RateLimitConfig 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_rate_limit_config() {
    let config = RateLimitConfig {
        requests_per_second: 10,
        orders_per_minute: 100,
        ws_messages_per_second: 5,
    };
    assert_eq!(config.requests_per_second, 10);
    assert_eq!(config.orders_per_minute, 100);
    assert_eq!(config.ws_messages_per_second, 5);
}

#[test]
fn test_rate_limit_config_serialization() {
    let config = RateLimitConfig {
        requests_per_second: 10,
        orders_per_minute: 100,
        ws_messages_per_second: 5,
    };
    let json = serde_json::to_string(&config).unwrap();
    let restored: RateLimitConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.requests_per_second, 10);
    assert_eq!(restored.orders_per_minute, 100);
}

// ═══════════════════════════════════════════════════════════════════════════
// ReconnectConfig 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_reconnect_config() {
    let config = ReconnectConfig {
        max_retries: 3,
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(30),
        backoff_multiplier: 2.0,
        circuit_breaker_threshold: 5,
        circuit_breaker_reset: Duration::from_secs(60),
    };
    assert_eq!(config.max_retries, 3);
    assert_eq!(config.initial_backoff, Duration::from_secs(1));
    assert_eq!(config.max_backoff, Duration::from_secs(30));
}

#[test]
fn test_reconnect_config_serialization() {
    let config = ReconnectConfig {
        max_retries: 5,
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(30),
        backoff_multiplier: 2.0,
        circuit_breaker_threshold: 3,
        circuit_breaker_reset: Duration::from_secs(60),
    };
    let json = serde_json::to_string(&config).unwrap();
    let restored: ReconnectConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.max_retries, 5);
    assert_eq!(restored.initial_backoff, Duration::from_secs(1));
}

// ═══════════════════════════════════════════════════════════════════════════
// ExchangeError 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_exchange_error_display() {
    let errors: Vec<ExchangeError> = vec![
        ExchangeError::ConnectionFailed("connection timeout".into()),
        ExchangeError::AuthenticationFailed("invalid signature".into()),
        ExchangeError::RateLimited { wait_ms: 1000 },
        ExchangeError::OrderRejected {
            reason: "insufficient balance".into(),
        },
        ExchangeError::OrderNotFound("order-123".into()),
        ExchangeError::ParseError("invalid json".into()),
        ExchangeError::WebSocket("connection closed".into()),
        ExchangeError::CircuitBreakerOpen,
    ];

    for err in errors {
        assert!(!err.to_string().is_empty());
    }
}

#[test]
fn test_exchange_error_connection_failed() {
    let err = ExchangeError::ConnectionFailed("connection refused".into());
    assert!(err.to_string().contains("connection refused"));
}

#[test]
fn test_exchange_error_authentication_failed() {
    let err = ExchangeError::AuthenticationFailed("bad api key".into());
    assert!(err.to_string().contains("bad api key"));
}

#[test]
fn test_exchange_error_rate_limited() {
    let err = ExchangeError::RateLimited { wait_ms: 5000 };
    assert!(err.to_string().contains("5000"));
}

#[test]
fn test_exchange_error_order_rejected() {
    let err = ExchangeError::OrderRejected {
        reason: "min notional".into(),
    };
    assert!(err.to_string().contains("min notional"));
}

#[test]
fn test_exchange_error_order_not_found() {
    let err = ExchangeError::OrderNotFound("order-456".into());
    assert!(err.to_string().contains("order-456"));
}

#[test]
fn test_exchange_error_insufficient_balance() {
    let err = ExchangeError::InsufficientBalance {
        required: rust_decimal::Decimal::from(1000),
        available: rust_decimal::Decimal::from(500),
    };
    assert!(err.to_string().contains("1000"));
    assert!(err.to_string().contains("500"));
}

#[test]
fn test_exchange_error_api_error() {
    let err = ExchangeError::ApiError {
        code: -1021,
        message: "Timestamp for this request is outside of the recvWindow".into(),
    };
    assert!(err.to_string().contains("-1021"));
    assert!(err.to_string().contains("recvWindow"));
}

#[test]
fn test_exchange_error_circuit_breaker_open() {
    let err = ExchangeError::CircuitBreakerOpen;
    assert!(err.to_string().contains("circuit breaker"));
}

// ═══════════════════════════════════════════════════════════════════════════
// ExchangeId Display 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_exchange_id_display() {
    assert_eq!(ExchangeId::Binance.to_string(), "binance");
    assert_eq!(ExchangeId::Okx.to_string(), "okx");
}

// ═══════════════════════════════════════════════════════════════════════════
// Symbol 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_symbol_creation() {
    let sym = axon_exchange::Symbol::new("BTCUSDT");
    assert_eq!(sym.0, "BTCUSDT");
}

#[test]
fn test_symbol_display() {
    let sym = axon_exchange::Symbol::new("BTCUSDT");
    assert_eq!(sym.to_string(), "BTCUSDT");
}

#[test]
fn test_symbol_serialization() {
    let sym = axon_exchange::Symbol::new("BTCUSDT");
    let json = serde_json::to_string(&sym).unwrap();
    let restored: axon_exchange::Symbol = serde_json::from_str(&json).unwrap();
    assert_eq!(sym, restored);
}

// ═══════════════════════════════════════════════════════════════════════════
// OrderId 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_id_creation() {
    let id = axon_exchange::OrderId::new();
    assert!(!id.to_string().is_empty());
}

#[test]
fn test_order_id_default() {
    let id = axon_exchange::OrderId::default();
    assert!(!id.to_string().is_empty());
}

#[test]
fn test_order_id_serialization() {
    let id = axon_exchange::OrderId::new();
    let json = serde_json::to_string(&id).unwrap();
    let restored: axon_exchange::OrderId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, restored);
}

// ═══════════════════════════════════════════════════════════════════════════
// Side 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_side_variants() {
    assert_ne!(axon_exchange::Side::Buy, axon_exchange::Side::Sell);
}

#[test]
fn test_side_serialization() {
    let sides = vec![axon_exchange::Side::Buy, axon_exchange::Side::Sell];
    for side in sides {
        let json = serde_json::to_string(&side).unwrap();
        let restored: axon_exchange::Side = serde_json::from_str(&json).unwrap();
        assert_eq!(side, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// OrderType 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_type_variants() {
    assert_ne!(
        axon_exchange::OrderType::Limit,
        axon_exchange::OrderType::Market
    );
    assert_ne!(
        axon_exchange::OrderType::Market,
        axon_exchange::OrderType::StopLoss
    );
    assert_ne!(
        axon_exchange::OrderType::StopLoss,
        axon_exchange::OrderType::StopLimit
    );
}

#[test]
fn test_order_type_serialization() {
    let types = vec![
        axon_exchange::OrderType::Limit,
        axon_exchange::OrderType::Market,
        axon_exchange::OrderType::StopLoss,
        axon_exchange::OrderType::StopLimit,
    ];
    for order_type in types {
        let json = serde_json::to_string(&order_type).unwrap();
        let restored: axon_exchange::OrderType = serde_json::from_str(&json).unwrap();
        assert_eq!(order_type, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// TimeInForce 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_time_in_force_variants() {
    assert_ne!(
        axon_exchange::TimeInForce::Gtc,
        axon_exchange::TimeInForce::Ioc
    );
    assert_ne!(
        axon_exchange::TimeInForce::Ioc,
        axon_exchange::TimeInForce::Fok
    );
}

#[test]
fn test_time_in_force_serialization() {
    let tifs = vec![
        axon_exchange::TimeInForce::Gtc,
        axon_exchange::TimeInForce::Ioc,
        axon_exchange::TimeInForce::Fok,
    ];
    for tif in tifs {
        let json = serde_json::to_string(&tif).unwrap();
        let restored: axon_exchange::TimeInForce = serde_json::from_str(&json).unwrap();
        assert_eq!(tif, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// OrderStatus 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_status_simple_variants() {
    assert_ne!(
        axon_exchange::OrderStatus::Pending,
        axon_exchange::OrderStatus::Sent
    );
    assert_ne!(
        axon_exchange::OrderStatus::Sent,
        axon_exchange::OrderStatus::Acknowledged
    );
}

#[test]
fn test_order_status_filled_variant() {
    let status = axon_exchange::OrderStatus::Filled {
        filled_qty: rust_decimal::Decimal::from(1),
        avg_price: rust_decimal::Decimal::from(50000),
    };
    assert!(matches!(status, axon_exchange::OrderStatus::Filled { .. }));
}

#[test]
fn test_order_status_cancelled_variant() {
    let status = axon_exchange::OrderStatus::Cancelled {
        filled_qty: rust_decimal::Decimal::ZERO,
    };
    assert!(matches!(
        status,
        axon_exchange::OrderStatus::Cancelled { .. }
    ));
}

#[test]
fn test_order_status_rejected_variant() {
    let status = axon_exchange::OrderStatus::Rejected {
        reason: "insufficient balance".into(),
    };
    assert!(matches!(
        status,
        axon_exchange::OrderStatus::Rejected { .. }
    ));
}

#[test]
fn test_order_status_serialization() {
    let statuses = vec![
        axon_exchange::OrderStatus::Pending,
        axon_exchange::OrderStatus::Sent,
        axon_exchange::OrderStatus::Acknowledged,
        axon_exchange::OrderStatus::Filled {
            filled_qty: rust_decimal::Decimal::from(1),
            avg_price: rust_decimal::Decimal::from(50000),
        },
        axon_exchange::OrderStatus::Cancelled {
            filled_qty: rust_decimal::Decimal::ZERO,
        },
    ];
    for status in statuses {
        let json = serde_json::to_string(&status).unwrap();
        let restored: axon_exchange::OrderStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MarginType 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_margin_type_variants() {
    assert_ne!(
        axon_exchange::MarginType::Isolated,
        axon_exchange::MarginType::Cross
    );
}

#[test]
fn test_margin_type_serialization() {
    let types = vec![
        axon_exchange::MarginType::Isolated,
        axon_exchange::MarginType::Cross,
    ];
    for margin_type in types {
        let json = serde_json::to_string(&margin_type).unwrap();
        let restored: axon_exchange::MarginType = serde_json::from_str(&json).unwrap();
        assert_eq!(margin_type, restored);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// OrderLifecycleManager 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_lifecycle_manager_creation() {
    let manager = axon_exchange::OrderLifecycleManager::new();
    assert_eq!(manager.active_count(), 0);
}

#[test]
fn test_lifecycle_manager_register_order() {
    let manager = axon_exchange::OrderLifecycleManager::new();
    let order = axon_exchange::Order {
        client_order_id: axon_exchange::OrderId::new(),
        symbol: axon_exchange::Symbol::new("BTCUSDT"),
        side: axon_exchange::Side::Buy,
        order_type: axon_exchange::OrderType::Limit,
        price: Some(rust_decimal::Decimal::from(50000)),
        quantity: rust_decimal::Decimal::from(1),
        time_in_force: axon_exchange::TimeInForce::Gtc,
        exchange: axon_exchange::ExchangeId::Binance,
        meta: std::collections::HashMap::new(),
    };
    let id = manager.register_order(order);
    assert!(!id.to_string().is_empty());
}

#[test]
fn test_lifecycle_manager_active_count() {
    let manager = axon_exchange::OrderLifecycleManager::new();
    assert_eq!(manager.active_count(), 0);

    let order = axon_exchange::Order {
        client_order_id: axon_exchange::OrderId::new(),
        symbol: axon_exchange::Symbol::new("BTCUSDT"),
        side: axon_exchange::Side::Buy,
        order_type: axon_exchange::OrderType::Limit,
        price: Some(rust_decimal::Decimal::from(50000)),
        quantity: rust_decimal::Decimal::from(1),
        time_in_force: axon_exchange::TimeInForce::Gtc,
        exchange: axon_exchange::ExchangeId::Binance,
        meta: std::collections::HashMap::new(),
    };
    manager.register_order(order);
    assert_eq!(manager.active_count(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// ExchangeId 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_exchange_id_clone() {
    let id = ExchangeId::Binance;
    let cloned = id;
    assert_eq!(id, cloned);
}

#[test]
fn test_exchange_id_hash() {
    use std::collections::HashMap;
    let mut map = HashMap::new();
    map.insert(ExchangeId::Binance, "binance");
    map.insert(ExchangeId::Okx, "okx");
    assert_eq!(map.get(&ExchangeId::Binance), Some(&"binance"));
}

// ═══════════════════════════════════════════════════════════════════════════
// Symbol 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_symbol_clone() {
    let sym = axon_exchange::Symbol::new("BTCUSDT");
    let cloned = sym.clone();
    assert_eq!(sym, cloned);
}

#[test]
fn test_symbol_eq() {
    let sym1 = axon_exchange::Symbol::new("BTCUSDT");
    let sym2 = axon_exchange::Symbol::new("BTCUSDT");
    let sym3 = axon_exchange::Symbol::new("ETHUSDT");
    assert_eq!(sym1, sym2);
    assert_ne!(sym1, sym3);
}

#[test]
fn test_symbol_hash() {
    use std::collections::HashMap;
    let sym = axon_exchange::Symbol::new("BTCUSDT");
    let mut map = HashMap::new();
    map.insert(sym.clone(), "test");
    assert_eq!(map.get(&sym), Some(&"test"));
}

// ═══════════════════════════════════════════════════════════════════════════
// OrderId 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_id_clone() {
    let id = axon_exchange::OrderId::new();
    let cloned = id;
    assert_eq!(id, cloned);
}

#[test]
fn test_order_id_hash() {
    use std::collections::HashMap;
    let id = axon_exchange::OrderId::new();
    let mut map = HashMap::new();
    map.insert(id, "test");
    assert_eq!(map.get(&id), Some(&"test"));
}

// ═══════════════════════════════════════════════════════════════════════════
// Order 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_creation() {
    let order = axon_exchange::Order {
        client_order_id: axon_exchange::OrderId::new(),
        symbol: axon_exchange::Symbol::new("BTCUSDT"),
        side: axon_exchange::Side::Buy,
        order_type: axon_exchange::OrderType::Limit,
        price: Some(rust_decimal::Decimal::from(50000)),
        quantity: rust_decimal::Decimal::from(1),
        time_in_force: axon_exchange::TimeInForce::Gtc,
        exchange: axon_exchange::ExchangeId::Binance,
        meta: std::collections::HashMap::new(),
    };
    assert_eq!(order.symbol.0, "BTCUSDT");
    assert_eq!(order.side, axon_exchange::Side::Buy);
}

#[test]
fn test_order_serialization() {
    let order = axon_exchange::Order {
        client_order_id: axon_exchange::OrderId::new(),
        symbol: axon_exchange::Symbol::new("ETHUSDT"),
        side: axon_exchange::Side::Sell,
        order_type: axon_exchange::OrderType::Market,
        price: None,
        quantity: rust_decimal::Decimal::from(10),
        time_in_force: axon_exchange::TimeInForce::Ioc,
        exchange: axon_exchange::ExchangeId::Okx,
        meta: std::collections::HashMap::new(),
    };
    let json = serde_json::to_string(&order).unwrap();
    let restored: axon_exchange::Order = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.symbol.0, "ETHUSDT");
    assert_eq!(restored.side, axon_exchange::Side::Sell);
}

#[test]
fn test_order_with_meta() {
    let mut meta = std::collections::HashMap::new();
    meta.insert("strategy".to_string(), "momentum".to_string());
    meta.insert("signal_id".to_string(), "sig-123".to_string());

    let order = axon_exchange::Order {
        client_order_id: axon_exchange::OrderId::new(),
        symbol: axon_exchange::Symbol::new("BTCUSDT"),
        side: axon_exchange::Side::Buy,
        order_type: axon_exchange::OrderType::Limit,
        price: Some(rust_decimal::Decimal::from(50000)),
        quantity: rust_decimal::Decimal::from(1),
        time_in_force: axon_exchange::TimeInForce::Gtc,
        exchange: axon_exchange::ExchangeId::Binance,
        meta,
    };
    assert_eq!(order.meta.get("strategy"), Some(&"momentum".to_string()));
    assert_eq!(order.meta.get("signal_id"), Some(&"sig-123".to_string()));
}

// ═══════════════════════════════════════════════════════════════════════════
// ExchangeError 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_exchange_error_debug() {
    let err = ExchangeError::ConnectionFailed("test".into());
    let debug_str = format!("{:?}", err);
    assert!(debug_str.contains("ConnectionFailed"));
}

#[test]
fn test_exchange_error_parse() {
    let err = ExchangeError::ParseError("invalid json".into());
    assert!(err.to_string().contains("invalid json"));
}

#[test]
fn test_exchange_error_web_socket() {
    let err = ExchangeError::WebSocket("connection closed".into());
    assert!(err.to_string().contains("connection closed"));
}
