//! 4-Agent 统一抽象:`DeclarativeAgentRunner`
//!
//! 把 `MarketAgent` / `RiskAgent` / `ExecutionAgent` / `AuditAgent` 抽象成
//! 同一 trait,让 `SwarmOrchestrator` 可以用同一种方式(`Arc<dyn Runner>`)
////! 管理异构 agent。
//!
//! ## 设计动机
//!
//! 0.3.0 P0 之前,4 Agent 是 `pub struct` + 各自的 `pub async fn handle_message` 方法,
//! orchestrator 拿不到统一句柄,主循环只能 `match agent.role()`,4 种 role 写 4 个分支。
//!
//! 抽 trait 后:
//! 1. **统一调度**:`Vec<Arc<dyn DeclarativeAgentRunner>>` 一把遍历
//! 2. **行为可测**:`MockRunner` 替身做集成测试
//! 3. **未来扩展**:加新角色只需 impl trait,不改 orchestrator
//!
//! ## 与 Rust `DeclarativeAgent` 的关系
//!
//! 注意:此 trait **不**等于 [crate::declarative_agent::DeclarativeAgent]。
//! - `DeclarativeAgent` 是 LLM 驱动的单实例智能体(Observe → Think → Act → Verify),
//!   返回 `HarnessResult`,由 `HarnessBridge.adjudicate()` 裁决后执行工具。
//! - `DeclarativeAgentRunner` 是 swarm 领域的多智能体抽象,接受 `AgentMessage`、
//!   返回 `RunnerOutput`(下游消息 / 裁决结果 / 状态)。
//!
//! 命名沿用 "Declarative" 是为了表示"agent 声明式表达意图,Harness/Orchestrator 负责路由与执行"的一致理念;
//! 两者无强制 type-level 绑定。
//!
//! ## RunnerOutput 设计
//!
//! ```text
//! RunnerOutput::None         → 心跳 / 关闭等无副作用消息
//! RunnerOutput::Forwarded    → 已通过 outbox 发出下游消息(0 个或多个)
//! RunnerOutput::Adjudicated  → 触发了 HarnessBridge 裁决(投票 / 工具调用)
//! ```
//!
//! 这 3 个变体对应 4 种 agent 的典型行为:
//! - MarketAgent:主要是 `None`(处理心跳),产 `Forwarded` 较少
//! - RiskAgent:收到 `ExecutionRequest` → 风控检查 → 产 `Forwarded(RiskAssessment)`
//! - ExecutionAgent:收到 `RiskAssessment(approved=true)` → 调 `PlaceOrderTool` → 产 `Adjudicated` 或 `Forwarded(ExecutionResult)`
//! - AuditAgent:收到任意 result → 写日志 → `None`

use async_trait::async_trait;

use crate::swarm::agent::{AgentId, AgentRole, AgentStatus};
use crate::swarm::error::SwarmError;
use crate::swarm::message::AgentMessage;

use axon_core::harness_types::HarnessResult;

/// Runner 单步执行结果
///
/// 描述 agent 收到一条 `AgentMessage` 后产生什么副作用 / 产出了什么消息。
#[derive(Debug, Clone)]
pub enum RunnerOutput {
    /// 无副作用 / 状态消息(Heartbeat / Shutdown 等)
    None,
    /// 已通过 outbox 转发下游消息(消息内容已发送,不在 output 中重复)
    Forwarded {
        /// 转发的下游消息数
        forwarded: usize,
    },
    /// 触发了 `HarnessBridge.adjudicate()` 裁决(用于统计 / 日志)
    Adjudicated {
        /// 裁决结果
        result: HarnessResult,
    },
}

/// Agent 统一 trait — Swarm 编排器通过此 trait 与 4 类 agent 交互
///
/// 实现约束:
/// - `id` / `role` / `status` 必须是廉价 getter(&self 即可),不允许修改内部状态
/// - `run_step` 必须可在 `&mut self` 下调用(便于 orchestrator 串行调度)
/// - 错误应优先用 `SwarmError` 的语义化变体,避免吞掉 panic
#[async_trait]
pub trait DeclarativeAgentRunner: Send + Sync {
    /// Agent 唯一标识(由构造时分配,不可变)
    fn id(&self) -> &AgentId;

    /// Agent 角色
    fn role(&self) -> AgentRole;

    /// 当前状态(`Idle` / `Thinking` / `Executing` / `Failed` 等)
    fn status(&self) -> AgentStatus;

    /// 处理一条消息,返回副作用摘要
    ///
    /// 实现者**应**在内部修改状态机(`Thinking` → `Idle`),
    /// 并通过自己的 outbox 发送下游消息(若有)。
    ///
    /// 错误:`SwarmError::MessageSendFailed`(outbox 满 / 关闭)
    async fn run_step(&mut self, msg: AgentMessage) -> Result<RunnerOutput, SwarmError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use crate::swarm::agent::AgentId;
    use crate::swarm::message::MessageContent;

    /// 测试用 MockRunner — 统计 run_step 调用次数
    struct MockRunner {
        id: AgentId,
        role: AgentRole,
        status: AgentStatus,
        call_count: Arc<AtomicUsize>,
    }

    impl MockRunner {
        fn new(role: AgentRole) -> Self {
            Self {
                id: AgentId::new(role),
                role,
                status: AgentStatus::Idle,
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl DeclarativeAgentRunner for MockRunner {
        fn id(&self) -> &AgentId {
            &self.id
        }
        fn role(&self) -> AgentRole {
            self.role
        }
        fn status(&self) -> AgentStatus {
            self.status
        }
        async fn run_step(&mut self, _msg: AgentMessage) -> Result<RunnerOutput, SwarmError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.status = AgentStatus::Thinking;
            Ok(RunnerOutput::None)
        }
    }

    /// Trait 必须 object-safe(可用 `dyn DeclarativeAgentRunner`)
    #[test]
    fn trait_is_object_safe() {
        let runner: Box<dyn DeclarativeAgentRunner> = Box::new(MockRunner::new(AgentRole::Market));
        assert_eq!(runner.role(), AgentRole::Market);
    }

    /// `Arc<dyn DeclarativeAgentRunner>` 跨线程共享可用(Send + Sync 约束)
    #[test]
    fn arc_dyn_runner_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<dyn DeclarativeAgentRunner>>();
    }

    /// `RunnerOutput` 三个变体可构造 + Debug 输出
    #[test]
    fn runner_output_variants_constructible() {
        let _ = RunnerOutput::None;
        let _ = RunnerOutput::Forwarded { forwarded: 1 };
        // Adjudicated variant 需要一个 HarnessResult — 实际场景才填
    }

    /// MockRunner 调一次 run_step 计数 +1
    #[tokio::test]
    async fn mock_runner_records_call_count() {
        let mut runner = MockRunner::new(AgentRole::Audit);
        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("test"),
            to: runner.id.clone(),
            correlation_id: None,
            content: MessageContent::Heartbeat,
            timestamp: 0,
        };
        let out = runner.run_step(msg).await.unwrap();
        assert!(matches!(out, RunnerOutput::None));
        assert_eq!(runner.call_count.load(Ordering::SeqCst), 1);
    }
}
