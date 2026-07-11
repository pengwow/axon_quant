//! 端到端测试:axon-harness 编排系统
//!
//! ## 3 个测试场景
//!
//! 1. `harness_default_policy_pass`:DefaultPolicy 在正常预算下通过裁决
//! 2. `harness_rbac_gate_allow_and_deny`:RBACToolGate 权限允许/拒绝
//! 3. `harness_circuit_breaker_trips`:CircuitBreaker 连续失败触发熔断
//!
//! 运行:`cargo test -p axon-harness --test e2e_harness_pipeline`

use axon_core::harness_types::{AgentIntent, TaskContext};
use axon_harness::{
    CircuitBreaker, CircuitBreakerConfig, DefaultPolicy, HarnessConfig, HarnessPolicy,
    RBACToolGate, ToolGate,
};
use std::collections::{HashMap, HashSet};

// ── helpers ────────────────────────────────────────────────────────────

fn test_config() -> HarnessConfig {
    HarnessConfig {
        max_steps: 50,
        max_tokens: 10000,
        timeout_secs: 300,
        green_zone_threshold: 0.6,
        yellow_zone_threshold: 0.8,
        red_zone_threshold: 0.95,
    }
}

fn test_intent(action: &str) -> AgentIntent {
    AgentIntent {
        action: action.to_string(),
        tool: None,
        params: serde_json::Value::Null,
        confidence: 0.8,
        reasoning: "test".into(),
        estimated_tokens: 100,
    }
}

fn test_context(tokens_used: u64) -> TaskContext {
    TaskContext {
        step: 1,
        tokens_used,
        task_description: "test".into(),
        current_agent: "test_agent".into(),
        started_at: 1000,
        metadata: serde_json::Value::Null,
    }
}

// ── 1. DefaultPolicy: 正常预算下通过裁决 ──────────────────────────────

#[test]
fn harness_default_policy_pass() {
    let policy = DefaultPolicy::new(test_config());

    // 50% 预算 → Green 区间 → Approved
    let intent = test_intent("analyze_market");
    let ctx = test_context(5000);
    let result = policy.adjudicate(&intent, &ctx);

    match result {
        axon_harness::types::Adjudication::Approved => {}
        other => panic!("Green 区间应 Approved,实为 {other:?}"),
    }
}

// ── 2. RBACToolGate: 权限允许/拒绝 ────────────────────────────────────

#[test]
fn harness_rbac_gate_allow_and_deny() {
    let mut permissions = HashMap::new();
    permissions.insert(
        "analyst".to_string(),
        ["read_data".to_string(), "compute_stats".to_string()]
            .into_iter()
            .collect(),
    );
    let gate = RBACToolGate::new(permissions, HashSet::new());

    // analyst 可以 read_data → Allow
    let result = gate.check("read_data", "analyst", &serde_json::Value::Null);
    assert!(matches!(result, axon_harness::types::GateResult::Allowed));

    // analyst 不能 execute_trade → Deny
    let result = gate.check("execute_trade", "analyst", &serde_json::Value::Null);
    assert!(matches!(
        result,
        axon_harness::types::GateResult::Denied { .. }
    ));
}

// ── 3. CircuitBreaker: 连续失败触发熔断 ───────────────────────────────

#[test]
fn harness_circuit_breaker_trips() {
    let config = CircuitBreakerConfig {
        max_consecutive_failures: 3,
        cooldown_seconds: 60,
        max_daily_loss_pct: 2.0,
        max_position_pct: 20.0,
        max_daily_trades: 100,
    };
    let cb = CircuitBreaker::new(config);

    // 初始状态 Closed
    assert_eq!(cb.state(), axon_harness::BreakerState::Closed);

    // 记录 2 次失败 → 仍 Closed
    cb.record_failure("test failure 1");
    cb.record_failure("test failure 2");
    assert_eq!(cb.state(), axon_harness::BreakerState::Closed);

    // 第 3 次失败 → Open
    cb.record_failure("test failure 3");
    assert_eq!(cb.state(), axon_harness::BreakerState::Open);

    // check() 在 Open 状态应返回 false
    assert!(!cb.check());
}
