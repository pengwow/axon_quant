//! RiskAgent - 风控 Agent

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::swarm::agent::{AgentId, AgentRole, AgentStatus};
use crate::swarm::agent_runner::{DeclarativeAgentRunner, RunnerOutput};
use crate::swarm::error::SwarmError;
use crate::swarm::message::{AgentMessage, MessageContent, RiskSignal, TradeOrder};

/// RiskAgent 配置
pub struct RiskAgentConfig {
    /// 最大单笔金额
    pub max_order_notional: f64,
    /// 最大持仓
    pub max_position: f64,
    /// 最大回撤
    pub max_drawdown: f64,
}

impl Default for RiskAgentConfig {
    fn default() -> Self {
        Self {
            max_order_notional: 50000.0,
            max_position: 100000.0,
            max_drawdown: 0.15,
        }
    }
}

/// RiskAgent - 风控 Agent
pub struct RiskAgent {
    id: AgentId,
    status: AgentStatus,
    config: RiskAgentConfig,
    #[allow(dead_code)]
    inbox: mpsc::Receiver<AgentMessage>,
    outbox: mpsc::Sender<AgentMessage>,
    /// 已发送消息计数(测试可观察)
    sent_count: usize,
}

impl RiskAgent {
    /// 创建新的 RiskAgent
    pub fn new(
        id: AgentId,
        config: RiskAgentConfig,
        inbox: mpsc::Receiver<AgentMessage>,
        outbox: mpsc::Sender<AgentMessage>,
    ) -> Self {
        Self {
            id,
            status: AgentStatus::Idle,
            config,
            inbox,
            outbox,
            sent_count: 0,
        }
    }

    /// 获取已发送消息数(测试 / 监控用)
    pub fn sent_count(&self) -> usize {
        self.sent_count
    }

    /// 获取 Agent ID
    pub fn id(&self) -> &AgentId {
        &self.id
    }

    /// 获取角色
    pub fn role(&self) -> AgentRole {
        AgentRole::Risk
    }

    /// 获取状态
    pub fn status(&self) -> AgentStatus {
        self.status
    }

    /// 检查订单风险
    pub fn check_order_risk(&self, order: &TradeOrder) -> RiskSignal {
        let notional = order.quantity * order.price.unwrap_or(0.0);

        let mut violations = Vec::new();

        // 检查单笔金额
        if notional > self.config.max_order_notional {
            violations.push(format!(
                "Order notional {} exceeds limit {}",
                notional, self.config.max_order_notional
            ));
        }

        // 检查数量
        if order.quantity <= 0.0 {
            violations.push("Order quantity must be positive".into());
        }

        RiskSignal {
            symbol: order.symbol.clone(),
            approved: violations.is_empty(),
            risk_score: if violations.is_empty() { 0.1 } else { 0.9 },
            violations,
        }
    }

    /// 处理消息
    pub async fn handle_message(&mut self, msg: AgentMessage) -> Result<(), SwarmError> {
        self.status = AgentStatus::Thinking;

        match msg.content {
            MessageContent::ExecutionRequest(order) => {
                let risk_signal = self.check_order_risk(&order);
                // 发送风险评估结果
                let response = AgentMessage {
                    id: crate::swarm::message::MessageId::new(),
                    from: self.id.clone(),
                    to: msg.from,
                    correlation_id: msg.correlation_id,
                    content: MessageContent::RiskAssessment(risk_signal),
                    timestamp: chrono::Utc::now().timestamp(),
                };
                let _ = self.outbox.send(response).await;
                self.sent_count += 1;
                self.status = AgentStatus::Idle;
            }
            MessageContent::Heartbeat => {
                self.status = AgentStatus::Idle;
            }
            MessageContent::Shutdown => {
                self.status = AgentStatus::Failed;
            }
            _ => {
                self.status = AgentStatus::Idle;
            }
        }

        Ok(())
    }
}

#[async_trait]
impl DeclarativeAgentRunner for RiskAgent {
    fn id(&self) -> &AgentId {
        &self.id
    }
    fn role(&self) -> AgentRole {
        AgentRole::Risk
    }
    fn status(&self) -> AgentStatus {
        self.status
    }
    async fn run_step(&mut self, msg: AgentMessage) -> Result<RunnerOutput, SwarmError> {
        // `ExecutionRequest` 会经 outbox 发出 `RiskAssessment`;其他消息无下游产出。
        // 用消息内容判断 forwarded 数,避免依赖 mpsc::Sender 的容量 / 计数 API。
        let forwarded = matches!(msg.content, MessageContent::ExecutionRequest(_)) as usize;
        self.handle_message(msg).await?;
        if forwarded > 0 {
            Ok(RunnerOutput::Forwarded { forwarded })
        } else {
            Ok(RunnerOutput::None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::agent::AgentId;
    use crate::swarm::message::{OrderSide, TradeOrder};

    #[test]
    fn test_risk_agent_creation() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("risk_0");
        let config = RiskAgentConfig::default();
        let agent = RiskAgent::new(id.clone(), config, rx, tx);

        assert_eq!(agent.id(), &id);
        assert_eq!(agent.role(), AgentRole::Risk);
        assert_eq!(agent.status(), AgentStatus::Idle);
    }

    #[test]
    fn test_risk_agent_check_order_approved() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("risk_0");
        let config = RiskAgentConfig::default();
        let agent = RiskAgent::new(id, config, rx, tx);

        let order = TradeOrder {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: "limit".into(),
            price: Some(50000.0),
            reason: "Test".into(),
        };

        let signal = agent.check_order_risk(&order);
        assert!(signal.approved);
        assert!(signal.violations.is_empty());
    }

    #[test]
    fn test_risk_agent_check_order_rejected() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("risk_0");
        let config = RiskAgentConfig {
            max_order_notional: 1000.0,
            max_position: 100000.0,
            max_drawdown: 0.15,
        };
        let agent = RiskAgent::new(id, config, rx, tx);

        let order = TradeOrder {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 1.0,
            order_type: "limit".into(),
            price: Some(50000.0), // 50000 > 1000 limit
            reason: "Test".into(),
        };

        let signal = agent.check_order_risk(&order);
        assert!(!signal.approved);
        assert!(!signal.violations.is_empty());
    }

    #[test]
    fn test_risk_agent_check_negative_quantity() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("risk_0");
        let config = RiskAgentConfig::default();
        let agent = RiskAgent::new(id, config, rx, tx);

        let order = TradeOrder {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: -1.0,
            order_type: "limit".into(),
            price: Some(50000.0),
            reason: "Test".into(),
        };

        let signal = agent.check_order_risk(&order);
        assert!(!signal.approved);
        assert!(signal.violations.iter().any(|v| v.contains("positive")));
    }

    /// `RiskAgent` 实现 `DeclarativeAgentRunner`:
    /// - 收到 `ExecutionRequest` → 产 `RunnerOutput::Forwarded{1}`(经 outbox 发出 RiskAssessment,sent_count +1)
    /// - 收到 `Heartbeat` → 产 `RunnerOutput::None`(sent_count 不变)
    #[tokio::test]
    async fn test_risk_agent_runner_trait_impl() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("risk_0");
        let config = RiskAgentConfig::default();
        let mut agent = RiskAgent::new(id, config, rx, tx);

        // 收到 ExecutionRequest
        let order = TradeOrder {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: "limit".into(),
            price: Some(50000.0),
            reason: "Test".into(),
        };
        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("orchestrator"),
            to: AgentId::from_string("risk_0"),
            correlation_id: None,
            content: MessageContent::ExecutionRequest(order),
            timestamp: 1000,
        };
        let out = agent.run_step(msg).await.unwrap();
        assert!(matches!(out, RunnerOutput::Forwarded { forwarded: 1 }));
        assert_eq!(agent.sent_count(), 1);
        assert_eq!(agent.status(), AgentStatus::Idle);

        // 收到 Heartbeat → None,sent_count 不变
        let hb = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("orchestrator"),
            to: AgentId::from_string("risk_0"),
            correlation_id: None,
            content: MessageContent::Heartbeat,
            timestamp: 1000,
        };
        let out = agent.run_step(hb).await.unwrap();
        assert!(matches!(out, RunnerOutput::None));
        assert_eq!(agent.sent_count(), 1); // 不变
    }
}
