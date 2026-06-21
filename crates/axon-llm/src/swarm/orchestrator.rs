//! Swarm 编排器 - Agent 生命周期管理

use std::collections::HashMap;

use tokio::sync::mpsc;

use super::agent::{AgentId, AgentRole, AgentStatus};
use super::error::SwarmError;
use super::message::{AgentMessage, MarketSignal, MessageContent, VoteProposal};
use super::vote::{ConsensusManager, VoteResponse};

/// Swarm 配置
#[derive(Debug, Clone)]
pub struct SwarmConfig {
    /// 每个角色的最大 Agent 数量
    pub max_agents_per_role: HashMap<AgentRole, usize>,
    /// 投票超时（毫秒）
    pub vote_timeout_ms: u64,
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

/// Swarm 编排器
pub struct SwarmOrchestrator {
    /// Agent 注册表
    agents: HashMap<AgentId, AgentHandle>,
    /// 共识管理器
    consensus: ConsensusManager,
    /// 配置
    config: SwarmConfig,
    #[allow(dead_code)]
    inbox: mpsc::Receiver<AgentMessage>,
    #[allow(dead_code)]
    outbox: mpsc::Sender<AgentMessage>,
}

impl SwarmOrchestrator {
    /// 创建新的 Swarm 编排器
    pub fn new(
        config: SwarmConfig,
        inbox: mpsc::Receiver<AgentMessage>,
        outbox: mpsc::Sender<AgentMessage>,
    ) -> Self {
        Self {
            agents: HashMap::new(),
            consensus: ConsensusManager::new(),
            config,
            inbox,
            outbox,
        }
    }

    /// 注册 Agent
    pub fn register_agent(&mut self, handle: AgentHandle) -> Result<(), SwarmError> {
        // 检查数量限制
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

    /// 获取 Agent 数量
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// 获取指定角色的 Agent 数量
    pub fn agent_count_by_role(&self, role: AgentRole) -> usize {
        self.agents.values().filter(|a| a.role == role).count()
    }

    /// 获取 Agent 状态
    pub fn agent_status(&self, agent_id: &AgentId) -> Option<AgentStatus> {
        self.agents.get(agent_id).map(|a| a.status)
    }

    /// 发送消息给指定 Agent
    pub async fn send_message(&self, msg: AgentMessage) -> Result<(), SwarmError> {
        if let Some(handle) = self.agents.get(&msg.to) {
            handle
                .sender
                .send(msg)
                .await
                .map_err(|e| SwarmError::MessageSendFailed(e.to_string()))?;
        } else {
            return Err(SwarmError::AgentNotFound(msg.to.as_str().to_string()));
        }
        Ok(())
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
        Ok(())
    }

    /// 发起投票
    pub fn create_vote(&mut self, proposal: VoteProposal) -> String {
        let proposal_id = proposal.proposal_id.clone();
        self.consensus.submit_proposal(proposal);
        proposal_id
    }

    /// 提交投票响应
    pub fn submit_vote(&mut self, response: VoteResponse) {
        self.consensus.submit_vote(response);
    }

    /// 获取共识管理器引用
    pub fn consensus(&self) -> &ConsensusManager {
        &self.consensus
    }

    /// 处理市场信号
    pub async fn handle_market_signal(&mut self, signal: MarketSignal) -> Result<(), SwarmError> {
        // 1. 创建投票提案
        let proposal = VoteProposal {
            proposal_id: format!("vote_{}", chrono::Utc::now().timestamp_millis()),
            proposal_type: super::message::VoteType::TradeDecision,
            content: format!("{} {}", signal.signal_type, signal.symbol),
            deadline_ms: self.config.vote_timeout_ms as i64,
        };
        let proposal_id = self.create_vote(proposal);

        // 2. 广播给 RiskAgent 和 ExecutionAgent
        let vote_content = MessageContent::VoteRequest(VoteProposal {
            proposal_id: proposal_id.clone(),
            proposal_type: super::message::VoteType::TradeDecision,
            content: format!("Vote on: {} {}", signal.signal_type, signal.symbol),
            deadline_ms: self.config.vote_timeout_ms as i64,
        });

        self.broadcast_to_role(AgentRole::Risk, vote_content.clone())
            .await?;
        self.broadcast_to_role(AgentRole::Execution, vote_content)
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::agent::AgentId;

    #[test]
    fn test_swarm_orchestrator_creation() {
        let (tx, rx) = mpsc::channel(100);
        let config = SwarmConfig::default();
        let orchestrator = SwarmOrchestrator::new(config, rx, tx);

        assert_eq!(orchestrator.agent_count(), 0);
    }

    #[test]
    fn test_register_agent() {
        let (tx, rx) = mpsc::channel(100);
        let (agent_tx, _agent_rx) = mpsc::channel(10);
        let config = SwarmConfig::default();
        let mut orchestrator = SwarmOrchestrator::new(config, rx, tx);

        let handle = AgentHandle {
            id: AgentId::from_string("market_0"),
            role: AgentRole::Market,
            status: AgentStatus::Idle,
            sender: agent_tx,
        };

        orchestrator.register_agent(handle).unwrap();
        assert_eq!(orchestrator.agent_count(), 1);
        assert_eq!(orchestrator.agent_count_by_role(AgentRole::Market), 1);
    }

    #[test]
    fn test_register_agent_max_reached() {
        let (tx, rx) = mpsc::channel(100);
        let config = SwarmConfig {
            max_agents_per_role: HashMap::from([(AgentRole::Market, 1)]),
            vote_timeout_ms: 5000,
        };
        let mut orchestrator = SwarmOrchestrator::new(config, rx, tx);

        let (agent_tx1, _agent_rx1) = mpsc::channel(10);
        let (agent_tx2, _agent_rx2) = mpsc::channel(10);

        orchestrator
            .register_agent(AgentHandle {
                id: AgentId::from_string("market_0"),
                role: AgentRole::Market,
                status: AgentStatus::Idle,
                sender: agent_tx1,
            })
            .unwrap();

        let result = orchestrator.register_agent(AgentHandle {
            id: AgentId::from_string("market_1"),
            role: AgentRole::Market,
            status: AgentStatus::Idle,
            sender: agent_tx2,
        });

        assert!(result.is_err());
    }

    #[test]
    fn test_unregister_agent() {
        let (tx, rx) = mpsc::channel(100);
        let (agent_tx, _agent_rx) = mpsc::channel(10);
        let config = SwarmConfig::default();
        let mut orchestrator = SwarmOrchestrator::new(config, rx, tx);

        let id = AgentId::from_string("market_0");
        orchestrator
            .register_agent(AgentHandle {
                id: id.clone(),
                role: AgentRole::Market,
                status: AgentStatus::Idle,
                sender: agent_tx,
            })
            .unwrap();

        let removed = orchestrator.unregister_agent(&id);
        assert!(removed.is_some());
        assert_eq!(orchestrator.agent_count(), 0);
    }

    #[test]
    fn test_create_vote() {
        let (tx, rx) = mpsc::channel(100);
        let config = SwarmConfig::default();
        let mut orchestrator = SwarmOrchestrator::new(config, rx, tx);

        let proposal = VoteProposal {
            proposal_id: "vote_001".into(),
            proposal_type: crate::swarm::message::VoteType::TradeDecision,
            content: "Buy BTC-USDT".into(),
            deadline_ms: 5000,
        };

        let proposal_id = orchestrator.create_vote(proposal);
        assert_eq!(proposal_id, "vote_001");
        assert!(orchestrator.consensus().get_proposal("vote_001").is_some());
    }
}
