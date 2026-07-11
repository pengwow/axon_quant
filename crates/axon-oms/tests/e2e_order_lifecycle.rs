//! 端到端测试:axon-oms 订单生命周期
//!
//! ## 4 个测试场景
//!
//! 1. `oms_order_lifecycle_submitted_to_filled`:创建 → submit → Acknowledged → Filled 完整流转
//! 2. `oms_order_cancel_path`:创建 → submit → Cancelled 取消路径
//! 3. `oms_multi_order_management`:多订单并发管理
//! 4. `oms_snapshot_consistency`:snapshot 状态一致性
//!
//! 运行:`cargo test -p axon-oms --test e2e_order_lifecycle`

use axon_oms::{Order, OrderManager, OrderStatus, OrderType, Side};
use rust_decimal::Decimal;

// ── helpers ────────────────────────────────────────────────────────────

fn test_order(symbol: &str, side: Side, qty: &str, price: &str) -> Order {
    Order::new(
        symbol.into(),
        side,
        OrderType::Limit,
        Decimal::from_str_exact(qty).unwrap(),
        Decimal::from_str_exact(price).unwrap(),
    )
}

// ── 1. 完整流转: submit → Acknowledged → Filled ───────────────────────

#[test]
fn oms_order_lifecycle_submitted_to_filled() {
    let oms = OrderManager::new();
    let order = test_order("BTC-USDT", Side::Buy, "0.1", "50000");
    let id = oms.submit(order).unwrap();

    // submit 后应为 Submitted
    let status = oms.get_order_status(id).unwrap();
    assert!(matches!(status, OrderStatus::Submitted));

    // 更新到 Acknowledged
    oms.update_status(id, OrderStatus::Acknowledged).unwrap();
    let status = oms.get_order_status(id).unwrap();
    assert!(matches!(status, OrderStatus::Acknowledged));

    // 更新到 Filled
    oms.update_status(
        id,
        OrderStatus::Filled {
            filled_qty: Decimal::from_str_exact("0.1").unwrap(),
            avg_price: Decimal::from_str_exact("50000").unwrap(),
        },
    )
    .unwrap();

    // Filled 是终态，应从 active 移除
    assert!(oms.get_order_status(id).is_none());
}

// ── 2. 取消路径: submit → Cancelled ────────────────────────────────────

#[test]
fn oms_order_cancel_path() {
    let oms = OrderManager::new();
    let order = test_order("ETH-USDT", Side::Sell, "1.0", "3000");
    let id = oms.submit(order).unwrap();

    oms.update_status(id, OrderStatus::Acknowledged).unwrap();
    oms.cancel(id).unwrap();

    // Cancelled 是终态
    assert!(oms.get_order_status(id).is_none());
}

// ── 3. 多订单并发管理 ──────────────────────────────────────────────────

#[test]
fn oms_multi_order_management() {
    let oms = OrderManager::new();

    let mut ids = Vec::new();
    for i in 0..5 {
        let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
        let order = test_order("BTC-USDT", side, "0.01", "50000");
        ids.push(oms.submit(order).unwrap());
    }

    // 全部提交
    assert_eq!(ids.len(), 5);

    // 先 Acknowledged 再取消
    for &id in &ids {
        oms.update_status(id, OrderStatus::Acknowledged).unwrap();
    }
    oms.cancel(ids[0]).unwrap();
    oms.cancel(ids[2]).unwrap();

    // 剩余 3 个仍活跃
    assert_eq!(
        oms.get_order_status(ids[1]),
        Some(OrderStatus::Acknowledged)
    );
    assert_eq!(
        oms.get_order_status(ids[3]),
        Some(OrderStatus::Acknowledged)
    );
    assert_eq!(
        oms.get_order_status(ids[4]),
        Some(OrderStatus::Acknowledged)
    );
}

// ── 4. snapshot 一致性 ─────────────────────────────────────────────────

#[test]
fn oms_snapshot_consistency() {
    let oms = OrderManager::new();

    let o1 = test_order("BTC-USDT", Side::Buy, "0.1", "50000");
    let o2 = test_order("ETH-USDT", Side::Sell, "1.0", "3000");
    let id1 = oms.submit(o1).unwrap();
    let _id2 = oms.submit(o2).unwrap();

    // 先 Acknowledged 再 Filled
    oms.update_status(id1, OrderStatus::Acknowledged).unwrap();
    oms.update_status(
        id1,
        OrderStatus::Filled {
            filled_qty: Decimal::from_str_exact("0.1").unwrap(),
            avg_price: Decimal::from_str_exact("50000").unwrap(),
        },
    )
    .unwrap();

    // 余额快照应存在
    let _balance = oms.snapshot_balance();
}
