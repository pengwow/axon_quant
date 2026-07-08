//! HarnessBridge 桥接器
//!
//! Rust 层和 Python 层的集成点。当所有组件为 None 时，
//! Agent 行为与原始 ReAct 循环完全一致（零侵入）。

use std::sync::Arc;

use axon_core::harness_types::{AgentIntent, TaskContext};

use crate::default_policy::DefaultPolicy;
use crate::policy::{BudgetGuard, HarnessPolicy, ToolGate};
use crate::rbac_gate::RBACToolGate;
use crate::simple_budget::SimpleBudgetGuard;
use crate::types::{Adjudication, BudgetState, BudgetZone, GateResult, HarnessConfig};

/// Harness 桥接器
///
/// 持有可选的策略组件。全部为 None 时，所有方法返回默认值（零侵入模式）。
#[derive(Clone)]
pub struct HarnessBridge {
    policy: Option<Arc<dyn HarnessPolicy>>,
    tool_gate: Option<Arc<dyn ToolGate>>,
    budget: Option<Arc<dyn BudgetGuard>>,
}

impl HarnessBridge {
    /// 构造全 None 的实例（零侵入模式）
    pub fn none() -> Self {
        Self {
            policy: None,
            tool_gate: None,
            budget: None,
        }
    }

    /// 构造新实例
    pub fn new(
        policy: Option<Arc<dyn HarnessPolicy>>,
        tool_gate: Option<Arc<dyn ToolGate>>,
        budget: Option<Arc<dyn BudgetGuard>>,
    ) -> Self {
        Self {
            policy,
            tool_gate,
            budget,
        }
    }

    /// 使用默认组件构造实例
    pub fn with_defaults(config: HarnessConfig) -> Self {
        let policy = DefaultPolicy::new(config.clone());
        let budget = SimpleBudgetGuard::new(config);
        let gate = RBACToolGate::default();

        Self {
            policy: Some(Arc::new(policy)),
            tool_gate: Some(Arc::new(gate)),
            budget: Some(Arc::new(budget)),
        }
    }

    /// 是否激活（至少有一个组件）
    pub fn is_active(&self) -> bool {
        self.policy.is_some() || self.tool_gate.is_some() || self.budget.is_some()
    }

    /// 裁决 Agent 意图
    ///
    /// 无 Harness 时返回 `Adjudication::Approved`
    pub fn adjudicate(&self, intent: &AgentIntent, ctx: &TaskContext) -> Adjudication {
        self.policy
            .as_ref()
            .map(|p| p.adjudicate(intent, ctx))
            .unwrap_or(Adjudication::Approved)
    }

    /// 检查任务是否可以继续
    ///
    /// 无 Harness 时返回 `true`
    pub fn can_proceed(&self, ctx: &TaskContext) -> bool {
        self.policy
            .as_ref()
            .map(|p| p.can_proceed(ctx))
            .unwrap_or(true)
    }

    /// 工具门控检查
    ///
    /// 无 Harness 时返回 `GateResult::Allowed`
    pub fn check_tool(&self, tool: &str, agent: &str, params: &serde_json::Value) -> GateResult {
        self.tool_gate
            .as_ref()
            .map(|g| g.check(tool, agent, params))
            .unwrap_or(GateResult::Allowed)
    }

    /// 记录工具调用
    ///
    /// 无 Harness 时空操作
    pub fn record_tool_call(
        &self,
        tool: &str,
        agent: &str,
        params: &serde_json::Value,
        result: &str,
    ) {
        if let Some(g) = &self.tool_gate {
            g.record_call(tool, agent, params, result);
        }
    }

    /// 消耗 Token
    ///
    /// 无 Harness 时返回 `BudgetZone::Green`
    pub fn consume_tokens(&self, tokens: u64, model: &str) -> BudgetZone {
        self.budget
            .as_ref()
            .map(|b| b.consume(tokens, model))
            .unwrap_or(BudgetZone::Green)
    }

    /// 是否已熔断
    ///
    /// 无 Harness 时返回 `false`
    pub fn is_circuit_break(&self) -> bool {
        self.budget
            .as_ref()
            .map(|b| b.is_circuit_break())
            .unwrap_or(false)
    }

    /// 获取预算快照
    ///
    /// 无 Harness 时返回 `None`
    pub fn budget_snapshot(&self) -> Option<BudgetState> {
        self.budget.as_ref().map(|b| b.snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_core::harness_types::{AgentIntent, TaskContext};

    fn test_intent() -> AgentIntent {
        AgentIntent {
            action: "test".into(),
            tool: None,
            params: serde_json::Value::Null,
            confidence: 0.5,
            reasoning: "test".into(),
            estimated_tokens: 100,
        }
    }

    fn test_ctx() -> TaskContext {
        TaskContext {
            step: 1,
            tokens_used: 100,
            task_description: "test".into(),
            current_agent: "market".into(),
            started_at: 1000,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn test_none_bridge_defaults() {
        let bridge = HarnessBridge::none();
        assert!(!bridge.is_active());
        assert_eq!(
            bridge.adjudicate(&test_intent(), &test_ctx()),
            Adjudication::Approved
        );
        assert!(bridge.can_proceed(&test_ctx()));
        assert_eq!(
            bridge.check_tool("any", "agent", &serde_json::Value::Null),
            GateResult::Allowed
        );
        assert_eq!(bridge.consume_tokens(1000, "gpt-4o"), BudgetZone::Green);
        assert!(!bridge.is_circuit_break());
        assert!(bridge.budget_snapshot().is_none());
    }

    #[test]
    fn test_none_bridge_record_noop() {
        let bridge = HarnessBridge::none();
        // 不应 panic
        bridge.record_tool_call("tool", "agent", &serde_json::Value::Null, "result");
    }

    #[test]
    fn test_with_defaults() {
        let config = HarnessConfig::default();
        let bridge = HarnessBridge::with_defaults(config);
        assert!(bridge.is_active());
        assert_eq!(
            bridge.adjudicate(&test_intent(), &test_ctx()),
            Adjudication::Approved
        );
        assert!(bridge.can_proceed(&test_ctx()));
    }
}
