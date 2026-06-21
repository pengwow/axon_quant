//! axon-oms 端到端测试

use axon_oms::{Order, OrderManager, OrderStatus, OrderType, Side};
use rust_decimal::Decimal;

// ═══════════════════════════════════════════════════════════════════════════
// OrderStatus 状态转换测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_status_valid_transitions() {
    // New → Submitted
    assert!(OrderStatus::New.can_transition_to(&OrderStatus::Submitted));
    // New → Rejected
    assert!(OrderStatus::New.can_transition_to(&OrderStatus::Rejected {
        reason: "test".into()
    }));
    // Submitted → Acknowledged
    assert!(OrderStatus::Submitted.can_transition_to(&OrderStatus::Acknowledged));
    // Submitted → Rejected
    assert!(
        OrderStatus::Submitted.can_transition_to(&OrderStatus::Rejected {
            reason: "test".into()
        })
    );
}

#[test]
fn test_order_status_invalid_transitions() {
    // New → Acknowledged (无效)
    assert!(!OrderStatus::New.can_transition_to(&OrderStatus::Acknowledged));
    // New → Filled (无效)
    assert!(!OrderStatus::New.can_transition_to(&OrderStatus::Filled {
        filled_qty: Decimal::from(1),
        avg_price: Decimal::from(50000),
    }));
    // Submitted → PartiallyFilled (无效)
    assert!(
        !OrderStatus::Submitted.can_transition_to(&OrderStatus::PartiallyFilled {
            filled_qty: Decimal::from(1),
            avg_price: Decimal::from(50000),
        })
    );
}

#[test]
fn test_order_status_fill_transitions() {
    // Acknowledged → PartiallyFilled
    assert!(
        OrderStatus::Acknowledged.can_transition_to(&OrderStatus::PartiallyFilled {
            filled_qty: Decimal::from(1),
            avg_price: Decimal::from(50000),
        })
    );
    // Acknowledged → Filled
    assert!(
        OrderStatus::Acknowledged.can_transition_to(&OrderStatus::Filled {
            filled_qty: Decimal::from(1),
            avg_price: Decimal::from(50000),
        })
    );
    // PartiallyFilled → Filled
    assert!(
        OrderStatus::PartiallyFilled {
            filled_qty: Decimal::from(1),
            avg_price: Decimal::from(50000),
        }
        .can_transition_to(&OrderStatus::Filled {
            filled_qty: Decimal::from(2),
            avg_price: Decimal::from(50000),
        })
    );
}

#[test]
fn test_order_status_cancel_transitions() {
    // Acknowledged → Cancelled
    assert!(
        OrderStatus::Acknowledged.can_transition_to(&OrderStatus::Cancelled {
            filled_qty: Decimal::ZERO,
        })
    );
    // PartiallyFilled → Cancelled
    assert!(
        OrderStatus::PartiallyFilled {
            filled_qty: Decimal::from(1),
            avg_price: Decimal::from(50000),
        }
        .can_transition_to(&OrderStatus::Cancelled {
            filled_qty: Decimal::from(1),
        })
    );
}

#[test]
fn test_order_status_rollback_transitions() {
    // Filled → Acknowledged (回滚)
    assert!(
        OrderStatus::Filled {
            filled_qty: Decimal::from(1),
            avg_price: Decimal::from(50000),
        }
        .can_transition_to(&OrderStatus::Acknowledged)
    );
    // PartiallyFilled → Acknowledged (回滚)
    assert!(
        OrderStatus::PartiallyFilled {
            filled_qty: Decimal::from(1),
            avg_price: Decimal::from(50000),
        }
        .can_transition_to(&OrderStatus::Acknowledged)
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// OrderManager 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_manager_submit() {
    let oms = OrderManager::new();
    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),   // 0.001
        Decimal::from(50000), // 50000
    );
    let id = oms.submit(order).unwrap();
    assert!(oms.get_order_status(id).is_some());
}

#[test]
fn test_order_manager_update_status() {
    let oms = OrderManager::new();
    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    );
    let id = oms.submit(order).unwrap();

    // submit 自动设为 Submitted
    let status = oms.get_order_status(id).unwrap();
    assert_eq!(status, OrderStatus::Submitted);

    // Submitted → Acknowledged
    oms.update_status(id, OrderStatus::Acknowledged).unwrap();
    let status = oms.get_order_status(id).unwrap();
    assert_eq!(status, OrderStatus::Acknowledged);
}

#[test]
fn test_order_manager_cancel() {
    let oms = OrderManager::new();
    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    );
    let id = oms.submit(order).unwrap();

    // Submitted → Acknowledged → Cancelled
    oms.update_status(id, OrderStatus::Acknowledged).unwrap();
    oms.cancel(id).unwrap();

    // 取消后订单从 active_orders 移除，active_count 为 0
    assert_eq!(oms.active_count(), 0);
}

#[test]
fn test_order_manager_fill() {
    let oms = OrderManager::new();
    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    );
    let id = oms.submit(order).unwrap();

    // Submitted → Acknowledged → Filled
    oms.update_status(id, OrderStatus::Acknowledged).unwrap();
    oms.update_status(
        id,
        OrderStatus::Filled {
            filled_qty: Decimal::new(1, 3),
            avg_price: Decimal::from(50000),
        },
    )
    .unwrap();

    // 填充后订单从 active_orders 移除，active_count 为 0
    assert_eq!(oms.active_count(), 0);
}

#[test]
fn test_order_manager_invalid_transition() {
    let oms = OrderManager::new();
    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    );
    let id = oms.submit(order).unwrap();

    // Submitted → Acknowledged (有效)
    oms.update_status(id, OrderStatus::Acknowledged).unwrap();

    // Acknowledged → Submitted (无效，不能回退到 Submitted)
    let result = oms.update_status(id, OrderStatus::Submitted);
    assert!(result.is_err());
}

#[test]
fn test_order_manager_snapshot_restore() {
    let oms = OrderManager::new();
    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    );
    let id = oms.submit(order).unwrap();

    // 创建快照
    let snapshot = oms.snapshot();
    assert!(!snapshot.active_orders.is_empty());

    // 恢复快照到新的 OMS
    let new_oms = OrderManager::new();
    new_oms.recover(snapshot).unwrap();
    assert!(new_oms.get_order_status(id).is_some());
}

#[test]
fn test_order_manager_active_count() {
    let oms = OrderManager::new();
    assert_eq!(oms.active_count(), 0);

    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    );
    oms.submit(order).unwrap();
    assert_eq!(oms.active_count(), 1);
}

#[test]
fn test_order_manager_history_count() {
    let oms = OrderManager::new();
    assert_eq!(oms.history_count(), 0);

    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    );
    oms.submit(order).unwrap();
    assert_eq!(oms.history_count(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// OrderStatus 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_status_is_terminal() {
    assert!(!OrderStatus::New.is_terminal());
    assert!(!OrderStatus::Submitted.is_terminal());
    assert!(!OrderStatus::Acknowledged.is_terminal());
    assert!(
        !OrderStatus::PartiallyFilled {
            filled_qty: Decimal::from(1),
            avg_price: Decimal::from(50000),
        }
        .is_terminal()
    );
    assert!(
        OrderStatus::Filled {
            filled_qty: Decimal::from(1),
            avg_price: Decimal::from(50000),
        }
        .is_terminal()
    );
    assert!(
        OrderStatus::Cancelled {
            filled_qty: Decimal::ZERO,
        }
        .is_terminal()
    );
    assert!(
        OrderStatus::Rejected {
            reason: "test".into(),
        }
        .is_terminal()
    );
}

#[test]
fn test_order_status_filled_quantity() {
    assert_eq!(OrderStatus::New.filled_quantity(), Decimal::ZERO);
    assert_eq!(OrderStatus::Submitted.filled_quantity(), Decimal::ZERO);
    assert_eq!(
        OrderStatus::PartiallyFilled {
            filled_qty: Decimal::from(5),
            avg_price: Decimal::from(50000),
        }
        .filled_quantity(),
        Decimal::from(5)
    );
    assert_eq!(
        OrderStatus::Filled {
            filled_qty: Decimal::from(10),
            avg_price: Decimal::from(50000),
        }
        .filled_quantity(),
        Decimal::from(10)
    );
    assert_eq!(
        OrderStatus::Cancelled {
            filled_qty: Decimal::from(3),
        }
        .filled_quantity(),
        Decimal::from(3)
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Order 扩展测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_new_defaults() {
    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    );
    assert_eq!(order.status, OrderStatus::New);
    assert!(order.idempotency_key.is_none());
    assert!(order.meta.is_empty());
}

#[test]
fn test_order_with_idempotency_key() {
    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    )
    .with_idempotency_key("key-123".into());
    assert_eq!(order.idempotency_key, Some("key-123".to_string()));
}

#[test]
fn test_order_transition_valid() {
    let mut order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    );
    assert!(order.transition(OrderStatus::Submitted).is_ok());
    assert_eq!(order.status, OrderStatus::Submitted);
}

#[test]
fn test_order_transition_invalid() {
    let mut order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Limit,
        Decimal::new(1, 3),
        Decimal::from(50000),
    );
    // New → Acknowledged (无效)
    assert!(order.transition(OrderStatus::Acknowledged).is_err());
}

#[test]
fn test_order_side_variants() {
    assert_ne!(Side::Buy, Side::Sell);
}

#[test]
fn test_order_type_variants() {
    assert_ne!(OrderType::Limit, OrderType::Market);
    assert_ne!(OrderType::Market, OrderType::StopLoss);
    assert_ne!(OrderType::StopLoss, OrderType::StopLimit);
}

// ═══════════════════════════════════════════════════════════════════════════
// OmsError 测试
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_oms_error_display() {
    let errors: Vec<axon_oms::OmsError> = vec![
        axon_oms::OmsError::OrderNotFound("order-123".into()),
        axon_oms::OmsError::InvalidTransition {
            from: "New".into(),
            to: "Filled".into(),
        },
        axon_oms::OmsError::DuplicateIdempotencyKey("key-123".into()),
    ];

    for err in errors {
        assert!(!err.to_string().is_empty());
    }
}
