//! Stage F 集成测试:`max_position_abs` 端到端拦截流程
//!
//! 验证 LLM agent 通过 PlaceOrderTool / ReplaceOrderTool / CancelOrderTool
//! 完整链路下,max_position_abs 风控在 mock 后端场景下正确生效。

use std::sync::Arc;

use axon_llm::tools::{Tool, ToolError};
use axon_llm::trading::mock::MockTradingBackend;
use axon_llm::trading::{
    CancelOrderTool, DailyCounter, OrderAck, OrderKind, OrderSide, PlaceOrderArgs, PlaceOrderTool,
    ReplaceOrderTool, RiskLimits, SafetyMode, TimeInForce,
};

/// 构造一份最小化的 Buy 限价单(0.1 BTC @ 50_000)
fn args_json(symbol: &str, side: &str, quantity: f64) -> String {
    serde_json::json!({
        "symbol": symbol,
        "side": side,
        "quantity": quantity,
        "order_type": "Limit",
        "price": 50_000.0
    })
    .to_string()
}

/// E2E 1:连续两次 Buy,第二次被 max_position_abs 拦截
///
/// mock 默认持仓 BTC-USDT 0.1,第一次 Buy 0.1 → projected=0.2 放行
/// 第二次 Buy 0.5 → projected=0.7 > max_abs=0.5 拒
#[tokio::test]
async fn e2e_buy_blocked_when_exceeds_max_position_abs() {
    let m = Arc::new(MockTradingBackend::new());
    let risk = RiskLimits {
        max_position_abs: Some(0.5),
        ..Default::default()
    };
    let tool = PlaceOrderTool::new(
        m.clone(),
        SafetyMode::Direct,
        risk,
        Arc::new(DailyCounter::default()),
    );

    // 第一次 Buy 0.1 → 成功(mock 默认持仓 0.1,projected=0.2 < 0.5)
    let s1 = tool
        .execute(&args_json("BTC-USDT", "Buy", 0.1))
        .await
        .unwrap();
    let ack1: OrderAck = serde_json::from_str(&s1).unwrap();
    assert_eq!(ack1.order_id, "MOCK-1");
    assert_eq!(m.order_count(), 1);

    // 第二次 Buy 0.5 → ToolError(mock apply_fill 后持仓 0.1 + 0.1 = 0.2,projected=0.7 > 0.5)
    let e = tool
        .execute(&args_json("BTC-USDT", "Buy", 0.5))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::ExecutionFailed(_)));
    // 第二次被风控拦截
    assert_eq!(m.order_count(), 1);
}

/// E2E 2:replace 改 quantity 扩大持仓 → 被 max_position_abs 拦截
#[tokio::test]
async fn e2e_replace_blocks_when_increases_position() {
    let m = Arc::new(MockTradingBackend::new());
    let place_tool = PlaceOrderTool::new(
        m.clone(),
        SafetyMode::Direct,
        RiskLimits::permissive(),
        Arc::new(DailyCounter::default()),
    );

    // 第一步:Buy 0.1
    let s1 = place_tool
        .execute(&args_json("BTC-USDT", "Buy", 0.1))
        .await
        .unwrap();
    let ack1: OrderAck = serde_json::from_str(&s1).unwrap();
    assert_eq!(m.order_count(), 1);

    // 第二步:用 replace 改 quantity 到 0.5
    let replace_tool = ReplaceOrderTool::new(
        m.clone(),
        RiskLimits {
            max_position_abs: Some(0.5),
            ..Default::default()
        },
    );
    let replace_args = serde_json::json!({
        "order_id": ack1.order_id,
        "new_req": {
            "symbol": "BTC-USDT", "side": "Buy", "quantity": 0.5,
            "order_type": "Limit", "price": 50_000.0
        }
    })
    .to_string();
    let e = replace_tool.execute(&replace_args).await.unwrap_err();
    assert!(matches!(e, ToolError::ExecutionFailed(_)));
    // 改单被风控拦截
    assert_eq!(m.order_count(), 1);
}

/// E2E 3:cancel 极严 max_position_abs 也能成功(cancel 不走位置检查)
#[tokio::test]
async fn e2e_cancel_succeeds_regardless_of_max_position_abs() {
    let m = Arc::new(MockTradingBackend::new());
    let place_tool = PlaceOrderTool::new(
        m.clone(),
        SafetyMode::Direct,
        RiskLimits::permissive(),
        Arc::new(DailyCounter::default()),
    );

    // 下 Buy 0.1
    let s1 = place_tool
        .execute(&args_json("BTC-USDT", "Buy", 0.1))
        .await
        .unwrap();
    let ack1: OrderAck = serde_json::from_str(&s1).unwrap();

    // 极严 max_position_abs=0.001,cancel 仍成功
    let cancel_tool = CancelOrderTool::new(
        m.clone(),
        RiskLimits {
            max_position_abs: Some(0.001),
            ..Default::default()
        },
        Arc::new(DailyCounter::default()),
    );
    let cancel_out = cancel_tool
        .execute(&format!(r#"{{"order_id":"{}"}}"#, ack1.order_id))
        .await
        .unwrap();
    let cancel_ack: OrderAck = serde_json::from_str(&cancel_out).unwrap();
    assert_eq!(cancel_ack.status.0, "Cancelled");
    // mock 内部状态
    assert!(m.cancelled_ids.lock().unwrap().contains(&ack1.order_id));
}

// ── 引用占位符(避免警告):让 PlaceOrderArgs / TimeInForce / OrderKind 也被使用 ──
#[allow(dead_code)]
fn _types_used() {
    let _args = PlaceOrderArgs {
        symbol: "BTC-USDT".into(),
        side: OrderSide::Buy,
        quantity: 0.1,
        order_type: OrderKind::Limit,
        price: Some(50_000.0),
        stop_loss: None,
        take_profit: None,
        time_in_force: TimeInForce::GTC,
        extras: serde_json::Value::Null,
    };
}
