//! ExecutionAgent - 执行 Agent

use tokio::sync::mpsc;

use crate::swarm::agent::{AgentId, AgentRole, AgentStatus};
use crate::swarm::error::SwarmError;
use crate::swarm::message::{AgentMessage, MessageContent, TradeOrder, TradeResult};

#[cfg(test)]
use crate::swarm::message::OrderSide;

/// ExecutionAgent 配置
pub struct ExecutionAgentConfig {
    /// 模拟延迟（毫秒）
    pub simulated_latency_ms: u64,
    /// 滑点（基点）
    pub slippage_bps: f64,
}

impl Default for ExecutionAgentConfig {
    fn default() -> Self {
        Self {
            simulated_latency_ms: 10,
            slippage_bps: 5.0,
        }
    }
}

/// ExecutionAgent - 执行 Agent
pub struct ExecutionAgent {
    id: AgentId,
    status: AgentStatus,
    config: ExecutionAgentConfig,
    #[allow(dead_code)]
    inbox: mpsc::Receiver<AgentMessage>,
    outbox: mpsc::Sender<AgentMessage>,
    /// 已执行订单计数
    executed_count: usize,
}

impl ExecutionAgent {
    /// 创建新的 ExecutionAgent
    pub fn new(
        id: AgentId,
        config: ExecutionAgentConfig,
        inbox: mpsc::Receiver<AgentMessage>,
        outbox: mpsc::Sender<AgentMessage>,
    ) -> Self {
        Self {
            id,
            status: AgentStatus::Idle,
            config,
            inbox,
            outbox,
            executed_count: 0,
        }
    }

    /// 获取 Agent ID
    pub fn id(&self) -> &AgentId {
        &self.id
    }

    /// 获取角色
    pub fn role(&self) -> AgentRole {
        AgentRole::Execution
    }

    /// 获取状态
    pub fn status(&self) -> AgentStatus {
        self.status
    }

    /// 获取已执行订单数
    pub fn executed_count(&self) -> usize {
        self.executed_count
    }

    /// 模拟执行订单
    pub fn execute_order(&mut self, order: &TradeOrder) -> TradeResult {
        self.status = AgentStatus::Executing;
        self.executed_count += 1;

        // 模拟滑点
        let slippage = self.config.slippage_bps / 10000.0;
        let _adjusted_price = order.price.unwrap_or(0.0) * (1.0 + slippage);

        // 模拟执行
        let order_id = format!("order_{}", self.executed_count);

        self.status = AgentStatus::Idle;

        TradeResult {
            order_id,
            success: true,
            error: None,
        }
    }

    /// 处理消息
    pub async fn handle_message(&mut self, msg: AgentMessage) -> Result<(), SwarmError> {
        self.status = AgentStatus::Thinking;

        match msg.content {
            MessageContent::ExecutionRequest(order) => {
                let result = self.execute_order(&order);

                // 发送执行结果
                let response = AgentMessage {
                    id: crate::swarm::message::MessageId::new(),
                    from: self.id.clone(),
                    to: msg.from,
                    correlation_id: msg.correlation_id,
                    content: MessageContent::ExecutionResult(result),
                    timestamp: chrono::Utc::now().timestamp(),
                };
                let _ = self.outbox.send(response).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::agent::AgentId;

    #[test]
    fn test_execution_agent_creation() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let config = ExecutionAgentConfig::default();
        let agent = ExecutionAgent::new(id.clone(), config, rx, tx);

        assert_eq!(agent.id(), &id);
        assert_eq!(agent.role(), AgentRole::Execution);
        assert_eq!(agent.status(), AgentStatus::Idle);
        assert_eq!(agent.executed_count(), 0);
    }

    #[test]
    fn test_execution_agent_execute_order() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let config = ExecutionAgentConfig::default();
        let mut agent = ExecutionAgent::new(id, config, rx, tx);

        let order = TradeOrder {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: "limit".into(),
            price: Some(50000.0),
            reason: "Test".into(),
        };

        let result = agent.execute_order(&order);
        assert!(result.success);
        assert!(!result.order_id.is_empty());
        assert_eq!(agent.executed_count(), 1);
    }

    #[test]
    fn test_execution_agent_multiple_orders() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("execution_0");
        let config = ExecutionAgentConfig::default();
        let mut agent = ExecutionAgent::new(id, config, rx, tx);

        let order = TradeOrder {
            symbol: "BTC-USDT".into(),
            side: OrderSide::Buy,
            quantity: 0.1,
            order_type: "limit".into(),
            price: Some(50000.0),
            reason: "Test".into(),
        };

        agent.execute_order(&order);
        agent.execute_order(&order);
        agent.execute_order(&order);

        assert_eq!(agent.executed_count(), 3);
    }
}
