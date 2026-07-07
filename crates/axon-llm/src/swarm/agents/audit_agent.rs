//! AuditAgent - 审计 Agent

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::swarm::agent::{AgentId, AgentRole, AgentStatus};
use crate::swarm::agent_runner::{DeclarativeAgentRunner, RunnerOutput};
use crate::swarm::error::SwarmError;
use crate::swarm::message::{AgentMessage, MessageContent};

/// 审计日志条目
#[derive(Debug, Clone)]
pub struct AuditEntry {
    /// 时间戳
    pub timestamp: i64,
    /// 来源 Agent
    pub source: String,
    /// 事件类型
    pub event_type: String,
    /// 事件详情
    pub details: String,
}

/// AuditAgent 配置
#[derive(Debug, Default)]
pub struct AuditAgentConfig {
    /// 是否启用详细日志
    pub verbose: bool,
}

/// AuditAgent - 审计 Agent
pub struct AuditAgent {
    id: AgentId,
    status: AgentStatus,
    config: AuditAgentConfig,
    #[allow(dead_code)]
    inbox: mpsc::Receiver<AgentMessage>,
    #[allow(dead_code)]
    outbox: mpsc::Sender<AgentMessage>,
    /// 审计日志
    audit_log: Vec<AuditEntry>,
}

impl AuditAgent {
    /// 创建新的 AuditAgent
    pub fn new(
        id: AgentId,
        config: AuditAgentConfig,
        inbox: mpsc::Receiver<AgentMessage>,
        outbox: mpsc::Sender<AgentMessage>,
    ) -> Self {
        Self {
            id,
            status: AgentStatus::Idle,
            config,
            inbox,
            outbox,
            audit_log: Vec::new(),
        }
    }

    /// 获取 Agent ID
    pub fn id(&self) -> &AgentId {
        &self.id
    }

    /// 获取角色
    pub fn role(&self) -> AgentRole {
        AgentRole::Audit
    }

    /// 获取状态
    pub fn status(&self) -> AgentStatus {
        self.status
    }

    /// 获取审计日志
    pub fn audit_log(&self) -> &[AuditEntry] {
        &self.audit_log
    }

    /// 记录事件
    pub fn log_event(&mut self, source: &str, event_type: &str, details: &str) {
        let entry = AuditEntry {
            timestamp: chrono::Utc::now().timestamp(),
            source: source.to_string(),
            event_type: event_type.to_string(),
            details: details.to_string(),
        };
        self.audit_log.push(entry);
    }

    /// 获取日志数量
    pub fn log_count(&self) -> usize {
        self.audit_log.len()
    }

    /// 处理消息
    pub async fn handle_message(&mut self, msg: AgentMessage) -> Result<(), SwarmError> {
        self.status = AgentStatus::Thinking;

        match msg.content {
            MessageContent::MarketAnalysis(signal) => {
                self.log_event(
                    msg.from.as_str(),
                    "MarketAnalysis",
                    &format!("{}: {}", signal.symbol, signal.reasoning),
                );
            }
            MessageContent::RiskAssessment(signal) => {
                self.log_event(
                    msg.from.as_str(),
                    "RiskAssessment",
                    &format!(
                        "{}: approved={}, violations={:?}",
                        signal.symbol, signal.approved, signal.violations
                    ),
                );
            }
            MessageContent::ExecutionResult(result) => {
                self.log_event(
                    msg.from.as_str(),
                    "ExecutionResult",
                    &format!("order_id={}, success={}", result.order_id, result.success),
                );
            }
            MessageContent::Heartbeat => {
                if self.config.verbose {
                    self.log_event(msg.from.as_str(), "Heartbeat", "");
                }
            }
            MessageContent::Shutdown => {
                self.log_event("system", "Shutdown", "AuditAgent shutting down");
                self.status = AgentStatus::Failed;
            }
            _ => {}
        }

        self.status = AgentStatus::Idle;
        Ok(())
    }
}

#[async_trait]
impl DeclarativeAgentRunner for AuditAgent {
    fn id(&self) -> &AgentId {
        &self.id
    }
    fn role(&self) -> AgentRole {
        AgentRole::Audit
    }
    fn status(&self) -> AgentStatus {
        self.status
    }
    async fn run_step(&mut self, msg: AgentMessage) -> Result<RunnerOutput, SwarmError> {
        // AuditAgent 只写 log,不发下游消息 → 永远 None
        self.handle_message(msg).await?;
        Ok(RunnerOutput::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::agent::AgentId;
    use crate::swarm::message::{MarketSignal, RiskSignal, SignalType, TradeResult};

    #[test]
    fn test_audit_agent_creation() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("audit_0");
        let config = AuditAgentConfig::default();
        let agent = AuditAgent::new(id.clone(), config, rx, tx);

        assert_eq!(agent.id(), &id);
        assert_eq!(agent.role(), AgentRole::Audit);
        assert_eq!(agent.status(), AgentStatus::Idle);
        assert_eq!(agent.log_count(), 0);
    }

    #[test]
    fn test_audit_agent_log_event() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("audit_0");
        let config = AuditAgentConfig::default();
        let mut agent = AuditAgent::new(id, config, rx, tx);

        agent.log_event("market_0", "Signal", "Buy BTC-USDT");
        assert_eq!(agent.log_count(), 1);
        assert_eq!(agent.audit_log()[0].source, "market_0");
    }

    #[tokio::test]
    async fn test_audit_agent_handle_market_analysis() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("audit_0");
        let config = AuditAgentConfig::default();
        let mut agent = AuditAgent::new(id, config, rx, tx);

        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("market_0"),
            to: AgentId::from_string("audit_0"),
            correlation_id: None,
            content: MessageContent::MarketAnalysis(MarketSignal {
                symbol: "BTC-USDT".into(),
                signal_type: SignalType::Buy,
                confidence: 0.85,
                reasoning: "Strong momentum".into(),
            }),
            timestamp: 1000,
        };

        agent.handle_message(msg).await.unwrap();
        assert_eq!(agent.log_count(), 1);
        assert_eq!(agent.audit_log()[0].event_type, "MarketAnalysis");
    }

    #[tokio::test]
    async fn test_audit_agent_handle_risk_assessment() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("audit_0");
        let config = AuditAgentConfig::default();
        let mut agent = AuditAgent::new(id, config, rx, tx);

        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("risk_0"),
            to: AgentId::from_string("audit_0"),
            correlation_id: None,
            content: MessageContent::RiskAssessment(RiskSignal {
                symbol: "BTC-USDT".into(),
                approved: true,
                risk_score: 0.1,
                violations: vec![],
            }),
            timestamp: 1000,
        };

        agent.handle_message(msg).await.unwrap();
        assert_eq!(agent.log_count(), 1);
        assert_eq!(agent.audit_log()[0].event_type, "RiskAssessment");
    }

    #[tokio::test]
    async fn test_audit_agent_handle_execution_result() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("audit_0");
        let config = AuditAgentConfig::default();
        let mut agent = AuditAgent::new(id, config, rx, tx);

        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("execution_0"),
            to: AgentId::from_string("audit_0"),
            correlation_id: None,
            content: MessageContent::ExecutionResult(TradeResult {
                order_id: "order_1".into(),
                success: true,
                error: None,
            }),
            timestamp: 1000,
        };

        agent.handle_message(msg).await.unwrap();
        assert_eq!(agent.log_count(), 1);
        assert_eq!(agent.audit_log()[0].event_type, "ExecutionResult");
    }

    /// `AuditAgent` 实现 `DeclarativeAgentRunner`:
    /// - 任意 `AgentMessage` → 产 `RunnerOutput::None`(只写 log,不发下游)
    /// - 触发后 log 计数 +1
    #[tokio::test]
    async fn test_audit_agent_runner_trait_impl() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("audit_0");
        let config = AuditAgentConfig {
            verbose: true, // 让 Heartbeat 也写 log
        };
        let mut agent = AuditAgent::new(id, config, rx, tx);

        assert_eq!(agent.log_count(), 0);

        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("orchestrator"),
            to: AgentId::from_string("audit_0"),
            correlation_id: None,
            content: MessageContent::Heartbeat,
            timestamp: 1000,
        };
        let out = agent.run_step(msg).await.unwrap();
        assert!(matches!(out, RunnerOutput::None));
        assert_eq!(agent.log_count(), 1);
        assert_eq!(agent.audit_log()[0].event_type, "Heartbeat");
    }
}
