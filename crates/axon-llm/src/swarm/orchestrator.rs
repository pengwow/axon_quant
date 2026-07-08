//! Swarm 编排器 - Agent 生命周期管理与消息路由
//!
//! ## 设计
//!
//! 0.3.0 P0 之前,`SwarmOrchestrator` 只管理静态 `AgentHandle` 注册表,无真正主循环。
//! 0.3.0 P0 之后:
//! - 用 `Arc<dyn DeclarativeAgentRunner>` 统一管理异构 agent
//! - `run_loop` 主循环:从 orchestrator inbox 收消息,按内容路由 + 触发投票/转发
//! - 各 agent 的 outbox 由 `register_agent_runner_with_channels` 接管,fan-in 到
//!   orchestrator 的同一个 inbox
//!
//! ## 消息路由表(run_loop 内)
//!
//! | 收到的消息             | 处理动作                                                                  |
//! |------------------------|---------------------------------------------------------------------------|
//! | `MarketAnalysis`       | 创建 `TradeDecision` 投票,广播 `VoteRequest` 给 Risk + Execution           |
//! | `RiskAssessment`       | `approved=true` → 转发给 Execution;`approved=false` → 广播给 Audit 记录  |
//! | `ExecutionResult`      | 广播给 Audit(记录执行结果)                                                |
//! | `VoteResult`           | 记录结果,`passed=true` → 转发给 Execution 生成 `ExecutionRequest`         |
//! | `VoteResponse`         | 投到 `ConsensusManager`,达到法定人数时回 `VoteResult`                      |
//! | `Shutdown`             | 设置 `shutdown_requested=true`,主循环退出                                   |
//! | `Heartbeat` / 其他     | 忽略                                                                      |

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;

use axon_core::harness_types::{AgentIntent, TaskContext};
use axon_harness::{Adjudication, HarnessBridge};

use super::agent::{AgentId, AgentRole, AgentStatus};
use super::agent_runner::DeclarativeAgentRunner;
use super::error::SwarmError;
use super::message::{
    AgentMessage, MarketSignal, MessageContent, OrderSide, TradeOrder, VoteProposal, VoteResult,
    VoteType,
};
use super::vote::{ConsensusManager, VoteResponse};

/// Swarm 配置
#[derive(Debug, Clone)]
pub struct SwarmConfig {
    /// 每个角色的最大 Agent 数量
    pub max_agents_per_role: HashMap<AgentRole, usize>,
    /// 投票超时（毫秒）
    pub vote_timeout_ms: u64,
    /// `run_loop` 单次 select 的最长等待时间(毫秒),到期后检查 shutdown 标志
    pub loop_tick_ms: u64,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        let mut max_agents = HashMap::new();
        max_agents.insert(AgentRole::Market, 3);
        max_agents.insert(AgentRole::Risk, 2);
        max_agents.insert(AgentRole::Execution, 1);
        max_agents.insert(AgentRole::Audit, 1);

        Self {
            max_agents_per_role: max_agents,
            vote_timeout_ms: 5000,
            loop_tick_ms: 100,
        }
    }
}

/// Agent 句柄
pub struct AgentHandle {
    /// Agent ID
    pub id: AgentId,
    /// 角色
    pub role: AgentRole,
    /// 状态
    pub status: AgentStatus,
    /// 发送通道
    pub sender: mpsc::Sender<AgentMessage>,
}

/// 已注册的 runner 条目:把 runner 句柄与它的 inbox sender 绑定
struct RegisteredRunner {
    id: AgentId,
    role: AgentRole,
    sender: mpsc::Sender<AgentMessage>,
    /// runner 句柄(保留以便将来直接 dispatch,目前主要用 sender 路由)
    #[allow(dead_code)]
    runner: Arc<dyn DeclarativeAgentRunner>,
}

/// `run_loop` 运行期统计
#[derive(Debug, Default, Clone)]
pub struct LoopStats {
    /// 处理的消息总数
    pub messages_processed: u64,
    /// MarketAnalysis 数
    pub market_signals: u64,
    /// RiskAssessment 数
    pub risk_assessments: u64,
    /// ExecutionResult 数
    pub execution_results: u64,
    /// 创建的投票数
    pub votes_created: u64,
    /// 完成的投票数(passed)
    pub votes_passed: u64,
    /// 完成的投票数(rejected)
    pub votes_rejected: u64,
    /// Harness 裁决 Approved 数
    pub harness_approved: u64,
    /// Harness 裁决 Rejected 数
    pub harness_rejected: u64,
    /// Harness 裁决 CircuitBreak 数
    pub harness_circuit_break: u64,
    /// Shutdown 触发次数
    pub shutdowns: u64,
}

/// Swarm 编排器
pub struct SwarmOrchestrator {
    /// 旧版 `AgentHandle` 注册表(兼容性保留)
    agents: HashMap<AgentId, AgentHandle>,
    /// Runner 注册表(0.3.0 P0 引入):`runner.inbox` 通过 sender 接受 orchestrator 消息
    runners: HashMap<AgentId, RegisteredRunner>,
    /// 共识管理器
    consensus: ConsensusManager,
    /// 共享的 Harness 桥接器(0.3.0 P0:`Arc` 包装,支持 orchestrator + 各 agent 同时持有)
    /// None 表示"零侵入模式",投票通过即执行。
    harness: Option<Arc<HarnessBridge>>,
    /// 配置
    config: SwarmConfig,
    /// `run_loop` 的 inbox:所有 agent outbox 都被 fan-in 汇入此处
    #[allow(dead_code)]
    inbox: Option<mpsc::Receiver<AgentMessage>>,
    /// `run_loop` 的 outbox(可选,用于把汇总结果投到外部)
    #[allow(dead_code)]
    outbox: Option<mpsc::Sender<AgentMessage>>,
    /// `run_loop` 统计
    stats: LoopStats,
    /// `run_loop` 收到 Shutdown 后置 true,主循环检查后退出
    shutdown_requested: bool,
}

impl SwarmOrchestrator {
    /// 创建新的 Swarm 编排器(无 inbox/outbox,无 harness)
    pub fn new(config: SwarmConfig) -> Self {
        Self {
            agents: HashMap::new(),
            runners: HashMap::new(),
            consensus: ConsensusManager::new(),
            harness: None,
            config,
            inbox: None,
            outbox: None,
            stats: LoopStats::default(),
            shutdown_requested: false,
        }
    }

    /// 创建带 Harness 桥接器的 Swarm 编排器
    pub fn with_harness(config: SwarmConfig, harness: HarnessBridge) -> Self {
        Self {
            agents: HashMap::new(),
            runners: HashMap::new(),
            consensus: ConsensusManager::new(),
            harness: Some(Arc::new(harness)),
            config,
            inbox: None,
            outbox: None,
            stats: LoopStats::default(),
            shutdown_requested: false,
        }
    }

    /// 创建带共享 Harness 桥接器的 Swarm 编排器(0.3.0 P0:`Arc<HarnessBridge>` 跨 owner 共享)
    ///
    /// 适用场景:同时把 `Arc` 传给多个 agent(每个 agent 持同一份,都能 `check_tool` / `adjudicate`)
    pub fn with_shared_harness(config: SwarmConfig, harness: Arc<HarnessBridge>) -> Self {
        Self {
            agents: HashMap::new(),
            runners: HashMap::new(),
            consensus: ConsensusManager::new(),
            harness: Some(harness),
            config,
            inbox: None,
            outbox: None,
            stats: LoopStats::default(),
            shutdown_requested: false,
        }
    }

    /// 设置 / 替换 Harness 桥接器(测试中常用)
    pub fn set_harness(&mut self, harness: HarnessBridge) {
        self.harness = Some(Arc::new(harness));
    }

    /// 获取共享 Harness 桥接器的 `Arc` 句柄
    ///
    /// 调用方可以 `Arc::clone(&orchestrator.shared_harness())` 传给 agent,
    /// 实现"orchestrator + 多 agent 同时持有同一份 harness"。
    pub fn shared_harness(&self) -> Option<Arc<HarnessBridge>> {
        self.harness.as_ref().map(Arc::clone)
    }

    /// 获取 harness 引用(单次借用版)
    pub fn harness(&self) -> Option<&HarnessBridge> {
        self.harness.as_deref()
    }

    /// 从投票结果构造 `AgentIntent`(供 `HarnessBridge.adjudicate()` 消费)
    ///
    /// proposal.content 形如 `"Buy BTC-USDT"` → action = `"Buy BTC-USDT"`,tool = `"place_order"`
    fn build_intent_from_vote(&self, vr: &VoteResult) -> AgentIntent {
        let content = self
            .consensus
            .get_proposal(&vr.proposal_id)
            .map(|p| p.content.clone())
            .unwrap_or_else(|| format!("vote {}", vr.proposal_id));
        AgentIntent {
            action: content.clone(),
            tool: Some("place_order".into()),
            params: serde_json::json!({
                "proposal_id": vr.proposal_id,
                "approve_count": vr.approve_count,
                "reject_count": vr.reject_count,
            }),
            confidence: if vr.passed { 0.8 } else { 0.3 },
            reasoning: format!("vote {} (passed={})", vr.proposal_id, vr.passed),
            estimated_tokens: 100,
        }
    }

    /// 旧版创建(保留 inbox/outbox)
    pub fn with_channels(
        config: SwarmConfig,
        inbox: mpsc::Receiver<AgentMessage>,
        outbox: mpsc::Sender<AgentMessage>,
    ) -> Self {
        Self {
            agents: HashMap::new(),
            runners: HashMap::new(),
            consensus: ConsensusManager::new(),
            harness: None,
            config,
            inbox: Some(inbox),
            outbox: Some(outbox),
            stats: LoopStats::default(),
            shutdown_requested: false,
        }
    }

    /// 注册一个 `DeclarativeAgentRunner`,并把它的 inbox sender 收下(供 run_loop 路由)
    ///
    /// 返回 orchestrator inbox 的 sender(调用方应把 agent 的 outbox 转发到这里):
    /// fan-in 模式 = `tokio::spawn` 多个 `outbox_rx.recv() -> orchestrator_inbox_tx.send(...)`
    pub fn register_agent_runner(
        &mut self,
        runner: Arc<dyn DeclarativeAgentRunner>,
        agent_inbox_tx: mpsc::Sender<AgentMessage>,
        orchestrator_inbox_tx: mpsc::Sender<AgentMessage>,
    ) -> Result<RegisteredOutboxHandle, SwarmError> {
        // 检查数量限制
        let role = runner.role();
        let current_count = self.runners.values().filter(|r| r.role == role).count();
        let max_count = self
            .config
            .max_agents_per_role
            .get(&role)
            .copied()
            .unwrap_or(1);
        if current_count >= max_count {
            return Err(SwarmError::MaxAgentsReached(role));
        }

        let id = runner.id().clone();
        self.runners.insert(
            id.clone(),
            RegisteredRunner {
                id: id.clone(),
                role,
                sender: agent_inbox_tx,
                runner,
            },
        );
        Ok(RegisteredOutboxHandle {
            agent_id: id,
            sender: orchestrator_inbox_tx,
        })
    }

    /// 注册 Agent(旧版,无 runner)
    pub fn register_agent(&mut self, handle: AgentHandle) -> Result<(), SwarmError> {
        let current_count = self
            .agents
            .values()
            .filter(|a| a.role == handle.role)
            .count();
        let max_count = self
            .config
            .max_agents_per_role
            .get(&handle.role)
            .copied()
            .unwrap_or(1);

        if current_count >= max_count {
            return Err(SwarmError::MaxAgentsReached(handle.role));
        }

        self.agents.insert(handle.id.clone(), handle);
        Ok(())
    }

    /// 注销 Agent
    pub fn unregister_agent(&mut self, agent_id: &AgentId) -> Option<AgentHandle> {
        self.agents.remove(agent_id)
    }

    /// 注销 Runner
    pub fn unregister_runner(&mut self, agent_id: &AgentId) {
        self.runners.remove(agent_id);
    }

    /// 获取已注册 runner 的 inbox sender 数量(测试可观察)
    pub fn runner_count(&self) -> usize {
        self.runners.len()
    }

    /// 获取 Agent 数量(旧版 + 新版 runner 之和)
    pub fn agent_count(&self) -> usize {
        self.agents.len() + self.runners.len()
    }

    /// 获取指定角色的 Agent 数量
    pub fn agent_count_by_role(&self, role: AgentRole) -> usize {
        let v1 = self.agents.values().filter(|a| a.role == role).count();
        let v2 = self.runners.values().filter(|r| r.role == role).count();
        v1 + v2
    }

    /// 查询 Agent 状态(0.3.0 P0 T2.9:支持 Python 绑定)
    ///
    /// 优先查 runner 表(runner 有 `status()`),回退到旧版 `AgentHandle`。
    /// 返回 `None` 表示 agent_id 不存在。
    pub fn agent_status(&self, agent_id: &AgentId) -> Option<AgentStatus> {
        if let Some(r) = self.runners.get(agent_id) {
            return Some(r.runner.status());
        }
        self.agents.get(agent_id).map(|h| h.status)
    }

    /// 发送消息给指定 Agent(runner 优先,回退到旧版 agents)
    pub async fn send_message(&self, msg: AgentMessage) -> Result<(), SwarmError> {
        if let Some(r) = self.runners.get(&msg.to) {
            r.sender
                .send(msg)
                .await
                .map_err(|e| SwarmError::MessageSendFailed(e.to_string()))?;
            return Ok(());
        }
        if let Some(handle) = self.agents.get(&msg.to) {
            handle
                .sender
                .send(msg)
                .await
                .map_err(|e| SwarmError::MessageSendFailed(e.to_string()))?;
            return Ok(());
        }
        Err(SwarmError::AgentNotFound(msg.to.as_str().to_string()))
    }

    /// 广播消息给所有指定角色的 Agent
    pub async fn broadcast_to_role(
        &self,
        role: AgentRole,
        content: MessageContent,
    ) -> Result<(), SwarmError> {
        for handle in self.agents.values().filter(|a| a.role == role) {
            let msg = AgentMessage {
                id: super::message::MessageId::new(),
                from: AgentId::from_string("orchestrator"),
                to: handle.id.clone(),
                correlation_id: None,
                content: content.clone(),
                timestamp: chrono::Utc::now().timestamp(),
            };
            let _ = handle.sender.send(msg).await;
        }
        for r in self.runners.values().filter(|r| r.role == role) {
            let msg = AgentMessage {
                id: super::message::MessageId::new(),
                from: AgentId::from_string("orchestrator"),
                to: r.id.clone(),
                correlation_id: None,
                content: content.clone(),
                timestamp: chrono::Utc::now().timestamp(),
            };
            let _ = r.sender.send(msg).await;
        }
        Ok(())
    }

    /// 发起投票
    pub fn create_vote(&mut self, proposal: VoteProposal) -> String {
        let proposal_id = proposal.proposal_id.clone();
        self.consensus.submit_proposal(proposal);
        self.stats.votes_created += 1;
        proposal_id
    }

    /// 提交投票响应
    pub fn submit_vote(&mut self, response: VoteResponse) -> Option<VoteResult> {
        let result = self.consensus.submit_vote(response);
        if let Some(r) = &result {
            if r.passed {
                self.stats.votes_passed += 1;
            } else {
                self.stats.votes_rejected += 1;
            }
        }
        result
    }

    /// 获取共识管理器引用
    pub fn consensus(&self) -> &ConsensusManager {
        &self.consensus
    }

    /// 清理已完成的投票
    pub fn cleanup_vote(&mut self, proposal_id: &str) {
        self.consensus.cleanup(proposal_id);
    }

    /// 获取统计信息
    pub fn stats(&self) -> &LoopStats {
        &self.stats
    }

    /// 触发 shutdown(外部调用)
    pub fn request_shutdown(&mut self) {
        self.shutdown_requested = true;
    }

    /// 是否已请求 shutdown
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested
    }

    /// 处理市场信号
    pub async fn handle_market_signal(&mut self, signal: MarketSignal) -> Result<(), SwarmError> {
        // 1. 创建投票提案
        let proposal = VoteProposal {
            proposal_id: format!("vote_{}", chrono::Utc::now().timestamp_millis()),
            proposal_type: VoteType::TradeDecision,
            content: format!("{} {}", signal.signal_type, signal.symbol),
            deadline_ms: self.config.vote_timeout_ms as i64,
        };
        let proposal_id = self.create_vote(proposal);

        // 2. 广播给 RiskAgent 和 ExecutionAgent
        let vote_content = MessageContent::VoteRequest(VoteProposal {
            proposal_id: proposal_id.clone(),
            proposal_type: VoteType::TradeDecision,
            content: format!("Vote on: {} {}", signal.signal_type, signal.symbol),
            deadline_ms: self.config.vote_timeout_ms as i64,
        });

        self.broadcast_to_role(AgentRole::Risk, vote_content.clone())
            .await?;
        self.broadcast_to_role(AgentRole::Execution, vote_content)
            .await?;

        Ok(())
    }

    // ═══════════════════════════════════════════════════════════════════════
    // run_loop 主循环
    // ═══════════════════════════════════════════════════════════════════════

    /// `run_loop`:从 `inbox_rx` 持续接收 AgentMessage,按内容路由 + 触发投票/转发
    ///
    /// ## 退出条件
    /// 1. 收到 `MessageContent::Shutdown` 消息
    /// 2. `request_shutdown()` 被外部调用
    /// 3. `inbox_rx` channel 关闭(所有 agent outbox 都 drop 了)
    ///
    /// ## 用法
    /// ```ignore
    /// let mut orchestrator = SwarmOrchestrator::new(SwarmConfig::default());
    /// let (tx, rx) = mpsc::channel(64);
    /// // ... 注册 runner,fan-in 它们的 outbox 到 tx ...
    /// orchestrator.run_loop(rx).await.unwrap();
    /// ```
    pub async fn run_loop(&mut self, mut inbox_rx: mpsc::Receiver<AgentMessage>) {
        let tick = std::time::Duration::from_millis(self.config.loop_tick_ms);
        loop {
            if self.shutdown_requested {
                break;
            }
            // select! 模拟:用 `tokio::time::timeout` 包裹 `recv`,
            // 收到 None(关闭)或 Shutdown 消息就退出
            let next = tokio::time::timeout(tick, inbox_rx.recv()).await;
            match next {
                Ok(Some(msg)) => {
                    if let Err(e) = self.dispatch(msg).await {
                        tracing::warn!("run_loop dispatch error: {e}");
                    }
                }
                Ok(None) => {
                    // channel 关闭
                    break;
                }
                Err(_) => {
                    // timeout:仅检查 shutdown 标志
                    continue;
                }
            }
        }
    }

    /// `run_loop` 的 Arc 共享版(0.3.0 P0 T2.9 引入)
    ///
    /// 与 `run_loop` 行为完全一致,接收 `Arc<Mutex<Self>>` 以支持跨 owner 持有
    /// (如 PyO3 绑定 / 跨 task 协调)。每条消息处理前 acquire `Mutex` 短锁,
    /// 与 `request_shutdown()` / `is_shutdown_requested()` 等其他方法并发安全。
    ///
    /// ## 用法
    /// ```ignore
    /// let orch = Arc::new(tokio::sync::Mutex::new(SwarmOrchestrator::new(cfg)));
    /// let orch_clone = Arc::clone(&orch);
    /// let (tx, rx) = mpsc::channel(64);
    /// tokio::spawn(async move {
    ///     SwarmOrchestrator::run_loop_arc(orch_clone, rx).await;
    /// });
    /// // ... 业务侧:tx.send(...)
    /// ```
    pub async fn run_loop_arc(
        orchestrator: Arc<TokioMutex<Self>>,
        mut inbox_rx: mpsc::Receiver<AgentMessage>,
    ) {
        let tick_dur = {
            let guard = orchestrator.lock().await;
            std::time::Duration::from_millis(guard.config.loop_tick_ms)
        };
        loop {
            // 短锁检查 shutdown
            {
                let guard = orchestrator.lock().await;
                if guard.shutdown_requested {
                    break;
                }
            }
            let next = tokio::time::timeout(tick_dur, inbox_rx.recv()).await;
            match next {
                Ok(Some(msg)) => {
                    let mut guard = orchestrator.lock().await;
                    if let Err(e) = guard.dispatch(msg).await {
                        tracing::warn!("run_loop_arc dispatch error: {e}");
                    }
                }
                Ok(None) => break,  // channel 关闭
                Err(_) => continue, // timeout,再次检查 shutdown
            }
        }
    }

    /// 单条消息路由(`run_loop` 内部使用,也可独立测试)
    pub async fn dispatch(&mut self, msg: AgentMessage) -> Result<(), SwarmError> {
        self.stats.messages_processed += 1;
        match msg.content {
            MessageContent::MarketAnalysis(signal) => {
                self.stats.market_signals += 1;
                self.handle_market_signal(signal).await
            }
            MessageContent::RiskAssessment(signal) => {
                self.stats.risk_assessments += 1;
                if signal.approved {
                    // 转发给 Execution agent:让 risk_assessment 触发的 ExecutionRequest
                    // (这里 orchestrator 本身不发 ExecutionRequest,而是把"approved"消息
                    // 转发给 execution agent,execution agent 据此调 PlaceOrderTool)
                    let exec_msg = AgentMessage {
                        id: super::message::MessageId::new(),
                        from: AgentId::from_string("orchestrator"),
                        to: AgentId::from_string("execution_0"),
                        correlation_id: msg.correlation_id,
                        content: MessageContent::RiskAssessment(signal),
                        timestamp: chrono::Utc::now().timestamp(),
                    };
                    // 找第一个 Execution agent
                    if let Some(r) = self
                        .runners
                        .values()
                        .find(|r| r.role == AgentRole::Execution)
                    {
                        r.sender
                            .send(exec_msg)
                            .await
                            .map_err(|e| SwarmError::MessageSendFailed(e.to_string()))?;
                    }
                } else {
                    // 不通过 → 通知 Audit 记录拒绝
                    self.broadcast_to_role(
                        AgentRole::Audit,
                        MessageContent::RiskAssessment(signal),
                    )
                    .await?;
                }
                Ok(())
            }
            MessageContent::ExecutionResult(result) => {
                self.stats.execution_results += 1;
                // 转发给 Audit
                self.broadcast_to_role(AgentRole::Audit, MessageContent::ExecutionResult(result))
                    .await?;
                Ok(())
            }
            MessageContent::VoteResponse(vr) => {
                if vr.passed {
                    // 投票通过 → 调 HarnessBridge.adjudicate() 做最终裁决
                    let intent = self.build_intent_from_vote(&vr);
                    let ctx = TaskContext {
                        step: 0,
                        tokens_used: 0,
                        task_description: format!("swarm vote {}", vr.proposal_id),
                        current_agent: "orchestrator".into(),
                        started_at: chrono::Utc::now().timestamp() as u64,
                        metadata: serde_json::Value::Null,
                    };
                    let adjudication = match &self.harness {
                        Some(h) => h.adjudicate(&intent, &ctx),
                        None => Adjudication::Approved, // 零侵入模式:投票通过即批准
                    };
                    match adjudication {
                        Adjudication::Approved => {
                            self.stats.harness_approved += 1;
                            // 转发给 Execution agent
                            if let Some(r) = self
                                .runners
                                .values()
                                .find(|r| r.role == AgentRole::Execution)
                            {
                                let order = TradeOrder {
                                    symbol: "BTC-USDT".into(),
                                    side: OrderSide::Buy,
                                    quantity: 0.1,
                                    order_type: "market".into(),
                                    price: None,
                                    reason: format!("harness approved: {}", vr.proposal_id),
                                };
                                let exec_msg = AgentMessage {
                                    id: super::message::MessageId::new(),
                                    from: AgentId::from_string("orchestrator"),
                                    to: r.id.clone(),
                                    correlation_id: Some(vr.proposal_id.clone()),
                                    content: MessageContent::ExecutionRequest(order),
                                    timestamp: chrono::Utc::now().timestamp(),
                                };
                                r.sender
                                    .send(exec_msg)
                                    .await
                                    .map_err(|e| SwarmError::MessageSendFailed(e.to_string()))?;
                            }
                        }
                        Adjudication::Rejected(reason) => {
                            self.stats.harness_rejected += 1;
                            tracing::info!("harness rejected vote {}: {}", vr.proposal_id, reason);
                            // 广播给 Audit 记录拒绝原因
                            self.broadcast_to_role(
                                AgentRole::Audit,
                                MessageContent::RiskAssessment(super::message::RiskSignal {
                                    symbol: "N/A".into(),
                                    approved: false,
                                    risk_score: 1.0,
                                    violations: vec![format!("harness rejected: {}", reason)],
                                }),
                            )
                            .await?;
                        }
                        Adjudication::CircuitBreak => {
                            self.stats.harness_circuit_break += 1;
                            tracing::warn!(
                                "harness circuit break on vote {}; requesting shutdown",
                                vr.proposal_id
                            );
                            // 熔断 → 触发 shutdown
                            self.shutdown_requested = true;
                        }
                        Adjudication::NeedRevision(reason) => {
                            // 需要修改 → 广播给 Market 重新分析(简化:仅记录)
                            tracing::info!(
                                "harness needs revision on vote {}: {}",
                                vr.proposal_id,
                                reason
                            );
                        }
                    }
                }
                // 清理投票(已统计)
                self.cleanup_vote(&vr.proposal_id);
                Ok(())
            }
            MessageContent::Shutdown => {
                self.stats.shutdowns += 1;
                self.shutdown_requested = true;
                Ok(())
            }
            _ => Ok(()), // Heartbeat / VoteRequest / ExecutionRequest 由 agent 内部处理
        }
    }
}

/// `register_agent_runner` 返回的 outbox handle:
/// - `sender`:orchestrator inbox 的 sender(调用方 spawn 把 agent outbox fan-in 到这里)
/// - `agent_id`:被注册 agent 的 ID(用于诊断)
pub struct RegisteredOutboxHandle {
    /// 被注册的 agent ID
    pub agent_id: AgentId,
    /// orchestrator inbox 的 sender(供 fan-in 任务 send)
    pub sender: mpsc::Sender<AgentMessage>,
}

/// 启动 fan-in 任务:把 `outbox_rx` 的每条消息转发到 `orchestrator_inbox_tx`
///
/// 调用方在每个 agent 注册后 `tokio::spawn` 一次。
/// 当 `outbox_rx` 关闭时任务自动退出(orchestrator inbox 也会随之收到 None)。
pub fn spawn_outbox_fanin(
    agent_id: AgentId,
    mut outbox_rx: mpsc::Receiver<AgentMessage>,
    orchestrator_inbox_tx: mpsc::Sender<AgentMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let label = agent_id.as_str().to_string();
        while let Some(msg) = outbox_rx.recv().await {
            if orchestrator_inbox_tx.send(msg).await.is_err() {
                tracing::debug!("outbox_fanin[{label}] orchestrator inbox closed");
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::agent::AgentId;

    #[test]
    fn test_swarm_orchestrator_creation() {
        let orchestrator = SwarmOrchestrator::new(SwarmConfig::default());
        assert_eq!(orchestrator.agent_count(), 0);
    }

    #[test]
    fn test_register_agent_legacy() {
        let mut orchestrator = SwarmOrchestrator::new(SwarmConfig::default());
        let (tx, _rx) = mpsc::channel(10);
        let handle = AgentHandle {
            id: AgentId::from_string("market_0"),
            role: AgentRole::Market,
            status: AgentStatus::Idle,
            sender: tx,
        };
        orchestrator.register_agent(handle).unwrap();
        assert_eq!(orchestrator.agent_count(), 1);
        assert_eq!(orchestrator.agent_count_by_role(AgentRole::Market), 1);
    }

    #[test]
    fn test_register_agent_max_reached() {
        let mut orchestrator = SwarmOrchestrator::new(SwarmConfig {
            max_agents_per_role: HashMap::from([(AgentRole::Market, 1)]),
            vote_timeout_ms: 5000,
            loop_tick_ms: 100,
        });
        let (tx1, _rx1) = mpsc::channel(10);
        let (tx2, _rx2) = mpsc::channel(10);

        orchestrator
            .register_agent(AgentHandle {
                id: AgentId::from_string("market_0"),
                role: AgentRole::Market,
                status: AgentStatus::Idle,
                sender: tx1,
            })
            .unwrap();
        let res = orchestrator.register_agent(AgentHandle {
            id: AgentId::from_string("market_1"),
            role: AgentRole::Market,
            status: AgentStatus::Idle,
            sender: tx2,
        });
        assert!(res.is_err());
    }

    #[test]
    fn test_create_vote() {
        let mut orchestrator = SwarmOrchestrator::new(SwarmConfig::default());
        let proposal = VoteProposal {
            proposal_id: "vote_001".into(),
            proposal_type: VoteType::TradeDecision,
            content: "Buy BTC-USDT".into(),
            deadline_ms: 5000,
        };
        let pid = orchestrator.create_vote(proposal);
        assert_eq!(pid, "vote_001");
        assert!(orchestrator.consensus().get_proposal("vote_001").is_some());
        assert_eq!(orchestrator.stats().votes_created, 1);
    }

    /// `run_loop` 收到 Shutdown 后退出
    #[tokio::test]
    async fn test_run_loop_exits_on_shutdown_message() {
        let mut orchestrator = SwarmOrchestrator::new(SwarmConfig {
            loop_tick_ms: 10,
            ..Default::default()
        });
        let (tx, rx) = mpsc::channel(8);

        let handle = tokio::spawn(async move {
            orchestrator.run_loop(rx).await;
        });

        tx.send(AgentMessage {
            id: super::super::message::MessageId::new(),
            from: AgentId::from_string("market_0"),
            to: AgentId::from_string("orchestrator"),
            correlation_id: None,
            content: MessageContent::Shutdown,
            timestamp: 0,
        })
        .await
        .unwrap();
        // drop sender → 触发 orchestrator 退出(若 Shutdown 没生效)
        drop(tx);
        // 给一点时间让 loop 处理
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), handle).await;
    }

    /// `run_loop` channel 关闭后退出
    #[tokio::test]
    async fn test_run_loop_exits_on_channel_close() {
        let mut orchestrator = SwarmOrchestrator::new(SwarmConfig {
            loop_tick_ms: 10,
            ..Default::default()
        });
        let (tx, rx) = mpsc::channel(8);
        drop(tx);
        // 短超时内 run_loop 必须返回(说明它检测到 channel 关闭并退出)
        let res = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            orchestrator.run_loop(rx),
        )
        .await;
        assert!(res.is_ok(), "run_loop should exit when channel closes");
    }

    /// `run_loop_arc`:Arc 共享版 — Shutdown 消息后退出,且其他 owner 可读 stats
    #[tokio::test]
    async fn test_run_loop_arc_handles_shutdown() {
        let orchestrator = Arc::new(TokioMutex::new(SwarmOrchestrator::new(SwarmConfig {
            loop_tick_ms: 10,
            ..Default::default()
        })));
        let (tx, rx) = mpsc::channel(8);
        let orch_clone = Arc::clone(&orchestrator);
        let handle = tokio::spawn(async move {
            SwarmOrchestrator::run_loop_arc(orch_clone, rx).await;
        });

        // 投 Shutdown 消息
        tx.send(AgentMessage {
            id: super::super::message::MessageId::new(),
            from: AgentId::from_string("market_0"),
            to: AgentId::from_string("orchestrator"),
            correlation_id: None,
            content: MessageContent::Shutdown,
            timestamp: 0,
        })
        .await
        .unwrap();
        drop(tx);

        // 200ms 内应退出
        let res = tokio::time::timeout(std::time::Duration::from_millis(200), handle).await;
        assert!(res.is_ok(), "run_loop_arc should exit on Shutdown");

        // 关闭后,从另一 owner 读 stats 仍可用
        let guard = orchestrator.lock().await;
        assert!(guard.is_shutdown_requested());
        assert_eq!(guard.stats().shutdowns, 1);
    }

    /// `run_loop_arc`:派发 MarketAnalysis 时创建投票(经 Arc 共享 self)
    #[tokio::test]
    async fn test_run_loop_arc_dispatches_market_signal() {
        let orchestrator = Arc::new(TokioMutex::new(SwarmOrchestrator::new(SwarmConfig {
            loop_tick_ms: 10,
            ..Default::default()
        })));
        let (tx, rx) = mpsc::channel(8);
        let orch_clone = Arc::clone(&orchestrator);
        let handle = tokio::spawn(async move {
            SwarmOrchestrator::run_loop_arc(orch_clone, rx).await;
        });

        tx.send(AgentMessage {
            id: super::super::message::MessageId::new(),
            from: AgentId::from_string("market_0"),
            to: AgentId::from_string("orchestrator"),
            correlation_id: None,
            content: MessageContent::MarketAnalysis(MarketSignal {
                symbol: "BTC-USDT".into(),
                signal_type: super::super::message::SignalType::Buy,
                confidence: 0.9,
                reasoning: "arc test".into(),
            }),
            timestamp: 0,
        })
        .await
        .unwrap();

        // 等 100ms 让 loop 处理
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // 触发 shutdown 让 loop 退出
        let orch_for_shutdown = Arc::clone(&orchestrator);
        {
            let mut g = orch_for_shutdown.lock().await;
            g.request_shutdown();
        }
        drop(tx);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;

        // 验证:market_signals 已被 dispatch
        let guard = orchestrator.lock().await;
        assert_eq!(guard.stats().market_signals, 1);
        assert_eq!(guard.stats().votes_created, 1);
    }

    /// `dispatch` 处理 `MarketAnalysis` 时创建投票
    #[tokio::test]
    async fn test_dispatch_market_analysis_creates_vote() {
        let mut orchestrator = SwarmOrchestrator::new(SwarmConfig::default());
        let msg = AgentMessage {
            id: super::super::message::MessageId::new(),
            from: AgentId::from_string("market_0"),
            to: AgentId::from_string("orchestrator"),
            correlation_id: None,
            content: MessageContent::MarketAnalysis(MarketSignal {
                symbol: "BTC-USDT".into(),
                signal_type: super::super::message::SignalType::Buy,
                confidence: 0.9,
                reasoning: "test".into(),
            }),
            timestamp: 0,
        };
        orchestrator.dispatch(msg).await.unwrap();
        assert_eq!(orchestrator.stats().market_signals, 1);
        assert_eq!(orchestrator.stats().votes_created, 1);
    }

    /// `dispatch` 处理 `Shutdown` 时设置 shutdown 标志
    #[tokio::test]
    async fn test_dispatch_shutdown_sets_flag() {
        let mut orchestrator = SwarmOrchestrator::new(SwarmConfig::default());
        let msg = AgentMessage {
            id: super::super::message::MessageId::new(),
            from: AgentId::from_string("market_0"),
            to: AgentId::from_string("orchestrator"),
            correlation_id: None,
            content: MessageContent::Shutdown,
            timestamp: 0,
        };
        orchestrator.dispatch(msg).await.unwrap();
        assert!(orchestrator.is_shutdown_requested());
        assert_eq!(orchestrator.stats().shutdowns, 1);
    }

    /// `dispatch` 处理通过的 `VoteResponse`:无 harness 时直接转发 Execution + harness_approved +1
    #[tokio::test]
    async fn test_dispatch_vote_response_passed_without_harness_approves() {
        use crate::swarm::agent_runner::RunnerOutput;
        use crate::swarm::message::MessageContent;
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// 简化 MockRunner:只暴露 id/role/status,run_step 返回 None
        struct MockExecutionRunner {
            id: AgentId,
            status: AgentStatus,
        }
        #[async_trait::async_trait]
        impl DeclarativeAgentRunner for MockExecutionRunner {
            fn id(&self) -> &AgentId {
                &self.id
            }
            fn role(&self) -> AgentRole {
                AgentRole::Execution
            }
            fn status(&self) -> AgentStatus {
                self.status
            }
            async fn run_step(&mut self, _msg: AgentMessage) -> Result<RunnerOutput, SwarmError> {
                Ok(RunnerOutput::None)
            }
        }

        let mut orchestrator = SwarmOrchestrator::new(SwarmConfig::default());
        // 注册 mock execution agent
        let (exec_inbox_tx, mut exec_inbox_rx) = mpsc::channel(8);
        let (orch_inbox_tx, _orch_inbox_rx) = mpsc::channel(8);
        let runner: Arc<dyn DeclarativeAgentRunner> = Arc::new(MockExecutionRunner {
            id: AgentId::from_string("execution_0"),
            status: AgentStatus::Idle,
        });
        orchestrator
            .register_agent_runner(runner, exec_inbox_tx, orch_inbox_tx)
            .unwrap();

        // 静默 unused 警告
        let _ = AtomicUsize::new(0).load(Ordering::SeqCst);

        // 先创建提案(让 build_intent_from_vote 能取到 content)
        orchestrator.create_vote(VoteProposal {
            proposal_id: "vote_passed_1".into(),
            proposal_type: VoteType::TradeDecision,
            content: "Buy BTC-USDT".into(),
            deadline_ms: 5000,
        });

        let msg = AgentMessage {
            id: super::super::message::MessageId::new(),
            from: AgentId::from_string("risk_0"),
            to: AgentId::from_string("orchestrator"),
            correlation_id: Some("vote_passed_1".into()),
            content: MessageContent::VoteResponse(VoteResult {
                proposal_id: "vote_passed_1".into(),
                passed: true,
                approve_count: 2,
                reject_count: 0,
                abstain_count: 0,
            }),
            timestamp: 0,
        };
        orchestrator.dispatch(msg).await.unwrap();

        // 零侵入模式:无 harness → Adjudication::Approved → 转发 Execution
        assert_eq!(orchestrator.stats().harness_approved, 1);
        assert_eq!(orchestrator.stats().harness_rejected, 0);
        // Execution agent 应该收到 ExecutionRequest
        let exec_msg =
            tokio::time::timeout(std::time::Duration::from_millis(200), exec_inbox_rx.recv())
                .await
                .expect("timeout")
                .expect("must have msg");
        assert!(matches!(
            exec_msg.content,
            MessageContent::ExecutionRequest(_)
        ));
        assert_eq!(exec_msg.correlation_id.as_deref(), Some("vote_passed_1"));
    }

    /// `dispatch` 处理通过的 `VoteResponse` + harness=None:不进入 harness_rejected 分支
    #[tokio::test]
    async fn test_dispatch_vote_response_not_passed_is_noop() {
        let mut orchestrator = SwarmOrchestrator::new(SwarmConfig::default());
        orchestrator.create_vote(VoteProposal {
            proposal_id: "vote_rej_1".into(),
            proposal_type: VoteType::TradeDecision,
            content: "Sell BTC-USDT".into(),
            deadline_ms: 5000,
        });
        let msg = AgentMessage {
            id: super::super::message::MessageId::new(),
            from: AgentId::from_string("risk_0"),
            to: AgentId::from_string("orchestrator"),
            correlation_id: Some("vote_rej_1".into()),
            content: MessageContent::VoteResponse(VoteResult {
                proposal_id: "vote_rej_1".into(),
                passed: false, // 未通过
                approve_count: 1,
                reject_count: 2,
                abstain_count: 0,
            }),
            timestamp: 0,
        };
        orchestrator.dispatch(msg).await.unwrap();
        // 未通过 → 不调 harness,不进 approved/rejected 分支
        assert_eq!(orchestrator.stats().harness_approved, 0);
        assert_eq!(orchestrator.stats().harness_rejected, 0);
    }

    /// `with_harness` + `harness()` getter:简单 setter/getter 路径
    #[test]
    fn test_with_harness_and_getter() {
        let bridge = HarnessBridge::none();
        let orchestrator = SwarmOrchestrator::with_harness(SwarmConfig::default(), bridge);
        assert!(orchestrator.harness().is_some());
        assert!(!orchestrator.harness().unwrap().is_active());
    }

    /// `with_shared_harness` + `shared_harness()` getter:
    /// 1. orchestrator 内部持 1 个 `Arc<HarnessBridge>`
    /// 2. 外部 clone 后可独立调用 `adjudicate` / `check_tool`
    /// 3. `Arc::strong_count` 反映共享
    #[test]
    fn test_shared_harness_multi_owner_sharing() {
        use std::sync::Arc;
        let bridge = Arc::new(HarnessBridge::none());
        let orchestrator =
            SwarmOrchestrator::with_shared_harness(SwarmConfig::default(), bridge.clone());

        // 1. orchestrator 持 1 + bridge 持 1 + orchestrator.shared_harness 返回 1 个 clone = 3
        // (Arc::clone 增加 strong_count)
        let external = orchestrator.shared_harness().expect("must have shared");
        assert_eq!(
            Arc::strong_count(&external),
            3,
            "expected 3 owners: original bridge var + orchestrator + external clone"
        );

        // 2. external 引用能直接调 HarnessBridge 方法
        let intent = AgentIntent {
            action: "test".into(),
            tool: None,
            params: serde_json::Value::Null,
            confidence: 0.5,
            reasoning: "test".into(),
            estimated_tokens: 100,
        };
        let ctx = TaskContext {
            step: 0,
            tokens_used: 0,
            task_description: "test".into(),
            current_agent: "external".into(),
            started_at: 0,
            metadata: serde_json::Value::Null,
        };
        let adj = external.adjudicate(&intent, &ctx);
        // HarnessBridge::none → Adjudication::Approved
        assert!(matches!(adj, Adjudication::Approved));

        // 3. clone 多次引用数累加
        let _e2 = orchestrator.shared_harness().unwrap();
        let _e3 = orchestrator.shared_harness().unwrap();
        assert_eq!(Arc::strong_count(&external), 5);
    }

    /// `set_harness` 替换 + `harness()` 仍可访问
    #[test]
    fn test_set_harness_replaces_existing() {
        let mut orchestrator = SwarmOrchestrator::new(SwarmConfig::default());
        assert!(orchestrator.harness().is_none());

        orchestrator.set_harness(HarnessBridge::none());
        assert!(orchestrator.harness().is_some());

        orchestrator.set_harness(HarnessBridge::none());
        assert!(orchestrator.harness().is_some());
    }
}
