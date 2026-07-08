//! Harness 核心 Trait 定义

use axon_core::harness_types::{AgentIntent, TaskContext};

use crate::types::{
    Adjudication, BudgetState, BudgetZone, CompressionHint, GateResult, ModelChoice,
};

/// 编排策略（最核心 trait）
pub trait HarnessPolicy: Send + Sync {
    /// 裁决 Agent 的声明式意图
    fn adjudicate(&self, intent: &AgentIntent, ctx: &TaskContext) -> Adjudication;

    /// 检查任务是否可以继续（安全阀：max_steps / max_tokens / timeout）
    fn can_proceed(&self, ctx: &TaskContext) -> bool;

    /// 根据当前预算区间选择模型
    fn select_model(&self, budget: &BudgetState) -> ModelChoice;

    /// 获取上下文压缩提示
    fn compression_hint(&self, budget: &BudgetState) -> CompressionHint;
}

/// 工具门控
pub trait ToolGate: Send + Sync {
    /// 工具调用前的门控检查（RBAC + Schema + 频率 + 风险）
    fn check(&self, tool: &str, agent: &str, params: &serde_json::Value) -> GateResult;

    /// 是否需要人工审批
    fn needs_approval(&self, tool: &str, params: &serde_json::Value) -> bool;

    /// 记录工具调用（审计）
    fn record_call(&self, tool: &str, agent: &str, params: &serde_json::Value, result: &str);
}

/// Token 预算守卫
pub trait BudgetGuard: Send + Sync {
    /// 消耗 Token，返回当前预算区间
    fn consume(&self, tokens: u64, model: &str) -> BudgetZone;

    /// 检查是否已熔断
    fn is_circuit_break(&self) -> bool;

    /// 获取剩余 Token
    fn remaining(&self) -> u64;

    /// 获取预算快照
    fn snapshot(&self) -> BudgetState;
}
