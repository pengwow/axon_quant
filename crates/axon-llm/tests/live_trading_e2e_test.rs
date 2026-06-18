//! Stage D Live Trading Demo E2E 集成测试
//!
//! 验证 `live_trading_e2e` demo 内部各组件的拼装:
//! 1. `RiskGate` trait 是 object-safe(`Arc<dyn RiskGate>` 可用)
//! 2. demo 内部 `CircuitBreakerGate` 桥接 `axon_risk::CircuitBreaker` 工作正常
//! 3. `PlaceOrderTool::with_gate` + `MockTradingBackend` 完整 pipeline
//!
//! **不调 LLM**:避免 LLM 行为不确定导致测试 flaky,只验证 demo 组件拼装。

#![cfg(feature = "trading-exchange")]

use std::sync::Arc;
use std::time::Duration;

use axon_llm::Tool;
use axon_llm::trading::mock::MockTradingBackend;
use axon_llm::trading::{DailyCounter, PlaceOrderTool, RiskGate, RiskLimits, SafetyMode};
use axon_risk::circuit_breaker::CircuitBreaker;

// ── 桥接适配器(与 demo 内部完全一致)──────────────────────

/// 桥接 `axon_risk::CircuitBreaker` 到 `axon_llm::RiskGate`
struct CircuitBreakerGate {
    cb: Arc<CircuitBreaker>,
}

impl RiskGate for CircuitBreakerGate {
    fn is_blocked(&self) -> Option<String> {
        if self.cb.is_active() {
            Some("circuit breaker active (cooldown 未结束)".to_string())
        } else {
            None
        }
    }
}

// ── 测试 ──────────────────────────────────────────────────

/// 编译期检查:`RiskGate` trait 是 object-safe(可作 `dyn RiskGate` 使用)
#[test]
fn risk_gate_trait_is_object_safe() {
    fn assert_obj_safe(_g: Arc<dyn RiskGate>) {}
    let g: Arc<dyn RiskGate> = Arc::new(CircuitBreakerGate {
        cb: Arc::new(CircuitBreaker::new(10.0, Duration::from_secs(60))),
    });
    assert_obj_safe(g);
}

/// demo pipeline 干跑(DryRun 模式):PlaceOrderTool DryRun + MockTradingBackend
#[tokio::test]
async fn demo_pipeline_dry_run_does_not_block() {
    let m = Arc::new(MockTradingBackend::new());
    let cb = Arc::new(CircuitBreaker::new(10.0, Duration::from_secs(60)));
    let gate: Arc<dyn RiskGate> = Arc::new(CircuitBreakerGate { cb: cb.clone() });

    let place = PlaceOrderTool::with_gate(
        m.clone(),
        SafetyMode::DryRun,
        RiskLimits::permissive(),
        Arc::new(DailyCounter::default()),
        gate,
    );

    // DryRun 多次:闸门即使被外部 trigger 也不影响 DryRun
    cb.check_and_trigger(-100.0);
    assert!(cb.is_active());

    let args = r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.001,"order_type":"Limit","price":50000.0}"#;
    for _ in 0..3 {
        let s = place.execute(args).await.expect("DryRun should succeed");
        let ack: serde_json::Value = serde_json::from_str(&s).expect("parse ack");
        assert_eq!(ack["status"], "DryRun");
    }
    // Mock 后端未被调(DryRun 路径)
    assert_eq!(m.order_count(), 0);
}

/// demo pipeline + CircuitBreaker:Direct 模式被闸门阻断
#[tokio::test]
async fn demo_pipeline_with_circuit_breaker_blocks_real_order() {
    let m = Arc::new(MockTradingBackend::new());
    let cb = Arc::new(CircuitBreaker::new(10.0, Duration::from_secs(60)));
    let gate: Arc<dyn RiskGate> = Arc::new(CircuitBreakerGate { cb: cb.clone() });

    let mut place = PlaceOrderTool::with_gate(
        m.clone(),
        SafetyMode::Direct,
        RiskLimits::permissive(),
        Arc::new(DailyCounter::default()),
        gate,
    );

    // 第一次:闸门未激活 → 通过
    let args = r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.001,"order_type":"Limit","price":50000.0}"#;
    let s1 = place
        .execute(args)
        .await
        .expect("first order should succeed");
    let ack1: serde_json::Value = serde_json::from_str(&s1).unwrap();
    assert_eq!(ack1["order_id"], "MOCK-1");
    assert_eq!(m.order_count(), 1);
    // 触发熔断器
    cb.check_and_trigger(-100.0);
    assert!(cb.is_active());

    // 第二次:闸门激活 → 阻断
    let e = place.execute(args).await.expect_err("should be blocked");
    let msg = format!("{:?}", e);
    assert!(msg.contains("gate blocked"), "msg = {}", msg);
    assert!(msg.contains("circuit breaker"), "msg = {}", msg);
    // Mock 后端仅被调一次(第二次被阻断)
    assert_eq!(m.order_count(), 1);

    // 测试 set_gate 运行时切换到 open gate → 恢复
    place.set_gate(Arc::new(axon_llm::trading::AlwaysOpenGate));
    let s3 = place
        .execute(args)
        .await
        .expect("after reset should succeed");
    let ack3: serde_json::Value = serde_json::from_str(&s3).unwrap();
    assert_eq!(ack3["order_id"], "MOCK-2");
    assert_eq!(m.order_count(), 2);
}

/// demo pipeline:TwoPhase 第二次被闸门阻断时 backend 不被调
#[tokio::test]
async fn demo_pipeline_two_phase_blocked_by_gate() {
    let m = Arc::new(MockTradingBackend::new());
    let cb = Arc::new(CircuitBreaker::new(10.0, Duration::from_secs(60)));
    let gate: Arc<dyn RiskGate> = Arc::new(CircuitBreakerGate { cb: cb.clone() });

    let place = PlaceOrderTool::with_gate(
        m.clone(),
        SafetyMode::TwoPhase,
        RiskLimits::permissive(),
        Arc::new(DailyCounter::default()),
        gate,
    );

    // 第一次:TwoPhase 暂存(闸门不检查)
    let args1 = r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.001,"order_type":"Limit","price":50000.0}"#;
    let s1 = place.execute(args1).await.expect("first should be pending");
    let ack1: serde_json::Value = serde_json::from_str(&s1).unwrap();
    assert_eq!(ack1["order_id"], "PENDING");
    let token = ack1["confirm_token"].as_str().expect("token").to_string();

    // 触发熔断器
    cb.check_and_trigger(-100.0);

    // 第二次带 token:被闸门阻断
    let args2 = format!(
        r#"{{"symbol":"BTC-USDT","side":"Buy","quantity":0.001,"order_type":"Limit","price":50000.0,"extras":{{"confirm_token":"{token}"}}}}"#
    );
    let e = place.execute(&args2).await.expect_err("should be blocked");
    let msg = format!("{:?}", e);
    assert!(msg.contains("gate blocked"), "msg = {}", msg);
    // Mock 后端未被调
    assert_eq!(m.order_count(), 0);
}

/// demo pipeline:风险触发后通过 set_gate 切换闸门,恢复下单能力
#[tokio::test]
async fn demo_pipeline_set_gate_allows_order_after_block() {
    let m = Arc::new(MockTradingBackend::new());
    let cb = Arc::new(CircuitBreaker::new(10.0, Duration::from_secs(60)));
    let gate: Arc<dyn RiskGate> = Arc::new(CircuitBreakerGate { cb: cb.clone() });

    let mut place = PlaceOrderTool::with_gate(
        m.clone(),
        SafetyMode::Direct,
        RiskLimits::permissive(),
        Arc::new(DailyCounter::default()),
        gate,
    );

    // 触发熔断器(无下单动作)
    cb.check_and_trigger(-100.0);
    assert!(cb.is_active());

    // 闸门阻断
    let args = r#"{"symbol":"BTC-USDT","side":"Buy","quantity":0.001,"order_type":"Limit","price":50000.0}"#;
    let e = place.execute(args).await.expect_err("blocked");
    assert!(format!("{:?}", e).contains("gate blocked"));

    // 模拟 cooldown 结束:reset + 切换闸门
    cb.reset();
    assert!(!cb.is_active());
    place.set_gate(Arc::new(axon_llm::trading::AlwaysOpenGate));

    // 现在能下单
    let s = place
        .execute(args)
        .await
        .expect("after reset should succeed");
    let ack: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(ack["order_id"], "MOCK-1");
    assert_eq!(m.order_count(), 1);
}
