//! 撤单/改单工具与 Mock backend 的 ReAct 集成测试
//!
//! 与 `tests/trading_integration.rs` 模式一致:用 `ScriptedMock` 替代真实 LLM,
//! 验证 Tool → Backend → Mock 的端到端链路。

use std::sync::Arc;

use axon_llm::tools::{Tool, ToolError};
use axon_llm::trading::mock::MockTradingBackend;
use axon_llm::trading::{
    CancelOrderTool, DailyCounter, OrderAck, OrderKind, OrderSide, PlaceOrderArgs,
    ReplaceOrderTool, RiskLimits, TimeInForce, TradingBackend,
};

fn mk_args() -> PlaceOrderArgs {
    PlaceOrderArgs {
        symbol: "BTC-USDT".into(),
        side: OrderSide::Buy,
        quantity: 0.05,
        order_type: OrderKind::Limit,
        price: Some(50_000.0),
        stop_loss: None,
        take_profit: None,
        time_in_force: TimeInForce::GTC,
        extras: serde_json::Value::Null,
    }
}

/// 1. cancel 走通完整链路:place_order → cancel_order → orders 看到 Cancelled
#[tokio::test]
async fn cancel_full_lifecycle_via_tool() {
    let m = Arc::new(MockTradingBackend::new());
    let tool = CancelOrderTool::new(
        m.clone(),
        RiskLimits::permissive(),
        Arc::new(DailyCounter::default()),
    );

    // place
    let ack = m.place_order(&mk_args()).await.unwrap();
    assert_eq!(ack.status.0, "Filled");

    // cancel via tool
    let args = format!(r#"{{"order_id":"{}"}}"#, ack.order_id);
    let out = tool.execute(&args).await.unwrap();
    let parsed: OrderAck = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed.status.0, "Cancelled");

    // mock 内部状态
    assert!(m.cancelled_ids.lock().unwrap().contains(&ack.order_id));
    assert_eq!(*m.cancel_count.lock().unwrap(), 1);
}

/// 2. replace 走通完整链路:place → replace → 看到新 quantity
#[tokio::test]
async fn replace_full_lifecycle_via_tool() {
    let m = Arc::new(MockTradingBackend::new());
    let tool = ReplaceOrderTool::new(m.clone(), RiskLimits::permissive());

    let ack = m.place_order(&mk_args()).await.unwrap();
    let args = format!(
        r#"{{"order_id":"{}","new_req":{{"symbol":"BTC-USDT","side":"Buy","quantity":0.3,"order_type":"Limit","price":51000.0}}}}"#,
        ack.order_id
    );
    let out = tool.execute(&args).await.unwrap();
    let parsed: OrderAck = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed.order_id, ack.order_id);
    assert_eq!(parsed.quantity, 0.3);
    assert_eq!(parsed.status.0, "Replaced");

    // mock 内部状态
    assert!(m.replaced_ids.lock().unwrap().contains(&ack.order_id));
}

/// 3. cancel 受 max_daily_cancels 限制
#[tokio::test]
async fn cancel_respects_daily_limit() {
    let m = Arc::new(MockTradingBackend::new());
    let tool = CancelOrderTool::new(
        m.clone(),
        RiskLimits {
            max_daily_cancels: Some(2),
            ..Default::default()
        },
        Arc::new(DailyCounter::default()),
    );

    let a1 = m.place_order(&mk_args()).await.unwrap();
    let a2 = m.place_order(&mk_args()).await.unwrap();
    let a3 = m.place_order(&mk_args()).await.unwrap();

    tool.execute(&format!(r#"{{"order_id":"{}"}}"#, a1.order_id))
        .await
        .unwrap();
    tool.execute(&format!(r#"{{"order_id":"{}"}}"#, a2.order_id))
        .await
        .unwrap();
    let e = tool
        .execute(&format!(r#"{{"order_id":"{}"}}"#, a3.order_id))
        .await
        .unwrap_err();
    match e {
        ToolError::ExecutionFailed(msg) => assert!(msg.contains("risk check failed")),
        other => panic!("expected ExecutionFailed, got {:?}", other),
    }
    // 只 cancel 了 2 笔
    assert_eq!(*m.cancel_count.lock().unwrap(), 2);
    // 第 3 笔的 cancel 计数不再增加
    assert_eq!(m.cancelled_ids.lock().unwrap().len(), 2);
}
