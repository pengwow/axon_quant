//! OmsSnapshot 向后兼容测试
//!
//! 验证:
//! - 老 OMS 生成的 snapshot(无 portfolio 字段)能被新 OMS 反序列化 + recover
//! - 新 OMS 生成的 snapshot(有 portfolio 段)能完整 round-trip

use axon_oms::{Fill, OmsSnapshot, Order, OrderManager, OrderStatus, OrderType, Side};
use pretty_assertions::assert_eq;
use rust_decimal_macros::dec;

#[test]
fn old_snapshot_without_portfolio_field_recovers() {
    // 模拟老 OMS 生成的 JSON(无 portfolio 字段)
    let old_json = r#"{
        "active_orders": {},
        "order_history": [],
        "version": 5,
        "timestamp": "2026-01-01T00:00:00Z"
    }"#;
    let snap: OmsSnapshot = serde_json::from_str(old_json).expect("deserialize old snapshot");
    let oms = OrderManager::new();
    oms.recover(snap).expect("recover old snapshot");

    // portfolio 应为空(老 snapshot 不携带 portfolio 信息)
    let positions = oms.snapshot_positions();
    assert!(positions.is_empty());
    let bal = oms.snapshot_balance();
    assert!(bal.cash.is_empty());
    // 关键:不报错(version / timestamp / active_orders 正确恢复)
    assert_eq!(oms.active_count(), 0);
    assert_eq!(oms.history_count(), 0);
}

#[test]
fn new_snapshot_round_trip_preserves_portfolio() {
    let oms = OrderManager::new();
    oms.deposit("USDT", dec!(100000));

    // 触发 fill 走 portfolio
    let order = Order::new(
        "BTC-USDT".into(),
        Side::Buy,
        OrderType::Market,
        dec!(0.5),
        dec!(50000),
    );
    let id = oms.submit(order).unwrap();
    oms.update_status(id, OrderStatus::Acknowledged).unwrap();
    oms.add_fill(
        id,
        Fill {
            fill_id: "f1".into(),
            instrument_id: "BTC-USDT".into(),
            instrument: None,
            price: dec!(50000),
            quantity: dec!(0.5),
            fee: dec!(0),
            timestamp: chrono::Utc::now(),
        },
    )
    .unwrap();

    let snap = oms.snapshot();
    let json = serde_json::to_string(&snap).expect("serialize");
    let snap2: OmsSnapshot = serde_json::from_str(&json).expect("deserialize");
    let oms2 = OrderManager::new();
    oms2.recover(snap2).expect("recover");

    assert_eq!(oms2.snapshot_balance().cash, oms.snapshot_balance().cash);
    assert_eq!(oms2.snapshot_positions(), oms.snapshot_positions());
}

#[test]
fn legacy_fill_with_symbol_key_deserializes() {
    // Fill.symbol → instrument_id 改名的兼容保证:老 snapshot / journal JSON 里
    // fill 的键是 "symbol",serde alias 必须读回;新 JSON 只输出 "instrument_id"。
    // 经 Value 往返构造旧键,不依赖 Decimal 的字符串表示细节。
    let fill = Fill {
        fill_id: "f1".into(),
        instrument_id: "BTC-USDT".into(),
        instrument: None,
        price: dec!(50000),
        quantity: dec!(0.5),
        fee: dec!(0),
        timestamp: chrono::Utc::now(),
    };
    let mut v = serde_json::to_value(&fill).unwrap();
    let obj = v.as_object_mut().unwrap();
    let val = obj.remove("instrument_id").expect("new key must exist");
    obj.insert("symbol".into(), val);
    let back: Fill =
        serde_json::from_value(v).expect("legacy symbol key must deserialize via alias");
    assert_eq!(back.instrument_id, "BTC-USDT");
}
