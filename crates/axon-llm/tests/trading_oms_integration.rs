//! `OmsTradingBackend` 集成测试(无 wiremock,直接打真实 OrderManager)。
//!
//! 验证端到端流程:LLM PlaceOrderArgs -> OmsTradingBackend -> OrderManager
//! -> OMS 状态机 + portfolio 状态变化。
//!
//! 与单元测试(`src/trading/oms.rs::tests`)的区别:
//! - **跨 crate 引用**:验证 `OmsTradingBackend` 作为 `pub` API 可被 crate 外访问。
//! - **真实 OrderManager**:不走 mock,直接调 `axon_oms::OrderManager` 真实 API。
//! - **完整 E2E 场景**:deposit / place_order / ack / fill / query / snapshot / recover。

#![cfg(feature = "trading-oms")]

use std::sync::Arc;

use axon_llm::trading::backend::TradingBackend;
use axon_llm::trading::oms::OmsTradingBackend;
use axon_llm::trading::{OrderKind, OrderSide, PlaceOrderArgs, TimeInForce};
use rust_decimal_macros::dec;
use serde_json::json;

/// 构造测试用 Limit Buy
fn mk_buy(symbol: &str, qty: f64, price: f64) -> PlaceOrderArgs {
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

/// 端到端:deposit -> place_order(register) -> query_balance
/// -> 推 fill 到 OMS -> query_positions + query_balance。
#[tokio::test]
async fn e2e_oms_backend_place_query_full_flow() {
    // 1. setup: deposit 25000 USDT(刚好够 0.5 * 50000)
    let manager = Arc::new(axon_oms::OrderManager::new());
    manager.deposit("USDT", dec!(25000));
    let backend = OmsTradingBackend::new(manager.clone());

    // 2. 初始 balance = 25000 USDT
    let bal = backend.get_balance().await.unwrap();
    let usdt = bal
        .currencies
        .iter()
        .find(|c| c.currency == "USDT")
        .expect("USDT currency");
    assert!((usdt.free - 25000.0).abs() < 1e-9);

    // 3. place_order: 0.5 BTC @ 50000
    let args = mk_buy("BTC-USDT", 0.5, 50000.0);
    let ack = backend.place_order(&args).await.unwrap();
    assert_eq!(ack.symbol, "BTC-USDT");
    assert_eq!(ack.status.0, "Submitted");
    // OMS active_count +1
    assert_eq!(manager.active_count(), 1);

    // 4. OMS 消费者推 fill(模拟撮合)
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

    // 5. balance 减少:25000 - 0.5*50000 = 0
    let bal = backend.get_balance().await.unwrap();
    let usdt_after = bal
        .currencies
        .iter()
        .find(|c| c.currency == "USDT")
        .expect("USDT currency");
    assert!(
        (usdt_after.free - 0.0).abs() < 1e-9,
        "cash 应为 0(fill 后),实际 free={}",
        usdt_after.free
    );

    // 6. positions 应包含 1 个 BTC-USDT
    let positions = backend.get_positions().await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].symbol, "BTC-USDT");
    assert!((positions[0].quantity - 0.5).abs() < 1e-9);
    assert!((positions[0].entry_price - 50000.0).abs() < 1e-9);
}

/// 完整成功路径:deposit 足够 -> place_order -> Acknowledged -> add_fill 成功
/// -> balance 减少 + position 增加。
#[tokio::test]
async fn e2e_oms_backend_full_fill_success_path() {
    let manager = Arc::new(axon_oms::OrderManager::new());
    manager.deposit("USDT", dec!(100000));
    let backend = OmsTradingBackend::new(manager.clone());

    let args = mk_buy("BTC-USDT", 0.1, 50000.0);
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
                quantity: dec!(0.1),
                fee: dec!(0),
                timestamp: chrono::Utc::now(),
            },
        )
        .unwrap();

    // balance = 100000 - 0.1 * 50000 = 95000
    let bal = backend.get_balance().await.unwrap();
    let usdt = bal
        .currencies
        .iter()
        .find(|c| c.currency == "USDT")
        .expect("USDT");
    assert!((usdt.free - 95000.0).abs() < 1e-9);

    // positions 包含 1 个 BTC-USDT, qty=0.1
    let positions = backend.get_positions().await.unwrap();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].symbol, "BTC-USDT");
    assert!((positions[0].quantity - 0.1).abs() < 1e-9);
    assert!((positions[0].entry_price - 50000.0).abs() < 1e-9);
}

/// 验证:OMS 崩溃后,snapshot + recover 可恢复 in-flight 订单。
#[tokio::test]
async fn e2e_oms_backend_snapshots_persist_pending_orders() {
    let manager = Arc::new(axon_oms::OrderManager::new());
    manager.deposit("USDT", dec!(10000));
    let backend = OmsTradingBackend::new(manager.clone());

    // 1. 提交 3 个订单
    for i in 0..3 {
        backend
            .place_order(&mk_buy("BTC-USDT", 0.001, 50000.0 + i as f64))
            .await
            .unwrap();
    }
    assert_eq!(manager.active_count(), 3);

    // 2. snapshot + 创建新 manager + recover
    let snap = manager.snapshot();
    let manager2 = Arc::new(axon_oms::OrderManager::new());
    manager2.recover(snap).unwrap();
    let backend2 = OmsTradingBackend::new(manager2.clone());
    assert_eq!(manager2.active_count(), 3);

    // 3. backend2 可继续 query
    let bal = backend2.get_balance().await.unwrap();
    let usdt = bal
        .currencies
        .iter()
        .find(|c| c.currency == "USDT")
        .expect("USDT");
    assert!((usdt.free - 10000.0).abs() < 1e-9);
}
