//! axon-harness 集成测试

use axon_core::harness_types::{AgentIntent, TaskContext};
use axon_harness::{
    Adjudication, BudgetGuard, BudgetZone, CircuitBreaker, CircuitBreakerConfig, DecisionRecord,
    GateResult, HarnessBridge, HarnessConfig, HarnessObserver, RBACToolGate, SimpleBudgetGuard,
    ToolGate,
};

fn test_config() -> HarnessConfig {
    HarnessConfig {
        max_steps: 50,
        max_tokens: 100_000,
        timeout_secs: 300,
        green_zone_threshold: 0.6,
        yellow_zone_threshold: 0.8,
        red_zone_threshold: 0.95,
    }
}

fn test_intent() -> AgentIntent {
    AgentIntent {
        action: "buy BTC".into(),
        tool: Some("place_order".into()),
        params: serde_json::json!({"symbol": "BTC", "qty": 0.1}),
        confidence: 0.85,
        reasoning: "bullish signal".into(),
        estimated_tokens: 2000,
    }
}

fn test_ctx() -> TaskContext {
    TaskContext {
        step: 1,
        tokens_used: 100,
        task_description: "execute trade".into(),
        current_agent: "execution".into(),
        started_at: 1000,
        metadata: serde_json::Value::Null,
    }
}

/// 测试 1: 完整 Agent 流程
#[test]
fn test_full_agent_flow() {
    let config = test_config();
    let bridge = HarnessBridge::with_defaults(config);

    // 1. 检查是否可以继续
    let ctx = test_ctx();
    assert!(bridge.can_proceed(&ctx));

    // 2. 裁决意图
    let intent = test_intent();
    assert_eq!(bridge.adjudicate(&intent, &ctx), Adjudication::Approved);

    // 3. 检查工具门控
    assert_eq!(
        bridge.check_tool("place_order", "execution", &intent.params),
        GateResult::NeedsApproval
    );

    // 4. 消耗 Token
    assert_eq!(bridge.consume_tokens(2000, "gpt-4o"), BudgetZone::Green);

    // 5. 检查预算快照
    let snap = bridge.budget_snapshot().unwrap();
    assert_eq!(snap.total_budget, 100_000);
    assert_eq!(snap.tokens_used, 2000);
}

/// 测试 2: 熔断器集成
#[test]
fn test_circuit_breaker_integration() {
    let cb_config = CircuitBreakerConfig {
        max_consecutive_failures: 3,
        cooldown_seconds: 0,
        max_daily_loss_pct: 5.0,
        max_position_pct: 20.0,
        max_daily_trades: 100,
    };
    let cb = CircuitBreaker::new(cb_config);

    // 1. 初始状态
    assert!(cb.check());
    assert!(!cb.is_open());

    // 2. 连续失败触发熔断
    cb.record_trade(-1.0, "BTC", 10.0);
    cb.record_trade(-1.0, "BTC", 10.0);
    assert!(cb.check()); // 2 次，未触发

    cb.record_trade(-1.0, "BTC", 10.0);
    assert!(!cb.check()); // 3 次，触发熔断
    assert!(cb.is_open());

    // 3. 强制重置
    cb.force_reset();
    assert!(cb.check());
    assert!(!cb.is_open());
}

/// 测试 3: 预算区间转换
#[test]
fn test_budget_zone_transitions() {
    let config = test_config();
    let guard = SimpleBudgetGuard::new(config);

    // 1. Green 区间
    assert_eq!(guard.consume(50_000, "gpt-4o"), BudgetZone::Green);
    assert_eq!(guard.remaining(), 50_000);

    // 2. Yellow 区间
    assert_eq!(guard.consume(30_000, "gpt-4o"), BudgetZone::Yellow);
    assert_eq!(guard.remaining(), 20_000);

    // 3. Red 区间
    assert_eq!(guard.consume(15_000, "gpt-4o"), BudgetZone::Red);
    assert_eq!(guard.remaining(), 5_000);

    // 4. 快照检查
    let snap = guard.snapshot();
    assert_eq!(snap.total_budget, 100_000);
    assert_eq!(snap.tokens_used, 95_000);
    assert_eq!(snap.zone, BudgetZone::Red);
}

/// 测试 4: 工具门控 + 审计
#[test]
fn test_tool_gate_and_audit() {
    let gate = RBACToolGate::default();

    // 1. 市场角色权限
    assert_eq!(
        gate.check("query_market", "market", &serde_json::Value::Null),
        GateResult::Allowed
    );
    assert!(matches!(
        gate.check("place_order", "market", &serde_json::Value::Null),
        GateResult::Denied(_)
    ));

    // 2. 执行角色权限
    assert_eq!(
        gate.check("place_order", "execution", &serde_json::Value::Null),
        GateResult::NeedsApproval
    );

    // 3. 审批检查
    assert!(gate.needs_approval("place_order", &serde_json::Value::Null));
    assert!(!gate.needs_approval("query_market", &serde_json::Value::Null));
}

/// 测试 5: 可观测性组件
#[test]
fn test_observer_metrics() {
    let mut observer = HarnessObserver::new();

    // 1. 记录决策
    observer.record_decision(DecisionRecord {
        timestamp: 1000,
        agent: "market".into(),
        action: "analyze".into(),
        adjudication: Adjudication::Approved,
        latency_ns: 100,
    });

    observer.record_decision(DecisionRecord {
        timestamp: 1001,
        agent: "execution".into(),
        action: "place_order".into(),
        adjudication: Adjudication::Rejected("low confidence".into()),
        latency_ns: 150,
    });

    // 2. 检查指标
    let metrics = observer.metrics();
    assert_eq!(metrics.total_decisions, 2);
    assert_eq!(metrics.approved_count, 1);
    assert_eq!(metrics.rejected_count, 1);
    assert_eq!(metrics.avg_latency_ns, 125);

    // 3. 检查最近决策
    let recent = observer.recent_decisions(1);
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].agent, "execution");
}
