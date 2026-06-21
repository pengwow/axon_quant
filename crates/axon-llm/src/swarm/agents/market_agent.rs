//! MarketAgent - 市场分析 Agent

use tokio::sync::mpsc;

use crate::swarm::agent::{AgentId, AgentRole, AgentStatus};
use crate::swarm::error::SwarmError;
use crate::swarm::message::{AgentMessage, MarketSignal, MessageContent, SignalType};

/// MarketAgent 配置
pub struct MarketAgentConfig {
    /// 分析的交易对
    pub symbols: Vec<String>,
    /// 信号阈值
    pub signal_threshold: f64,
}

impl Default for MarketAgentConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTC-USDT".into()],
            signal_threshold: 0.7,
        }
    }
}

/// MarketAgent - 市场分析 Agent
pub struct MarketAgent {
    id: AgentId,
    status: AgentStatus,
    config: MarketAgentConfig,
    #[allow(dead_code)]
    inbox: mpsc::Receiver<AgentMessage>,
    #[allow(dead_code)]
    outbox: mpsc::Sender<AgentMessage>,
}

impl MarketAgent {
    /// 创建新的 MarketAgent
    pub fn new(
        id: AgentId,
        config: MarketAgentConfig,
        inbox: mpsc::Receiver<AgentMessage>,
        outbox: mpsc::Sender<AgentMessage>,
    ) -> Self {
        Self {
            id,
            status: AgentStatus::Idle,
            config,
            inbox,
            outbox,
        }
    }

    /// 获取 Agent ID
    pub fn id(&self) -> &AgentId {
        &self.id
    }

    /// 获取角色
    pub fn role(&self) -> AgentRole {
        AgentRole::Market
    }

    /// 获取状态
    pub fn status(&self) -> AgentStatus {
        self.status
    }

    /// 获取配置的交易对
    pub fn symbols(&self) -> &[String] {
        &self.config.symbols
    }

    /// 处理消息
    pub async fn handle_message(&mut self, msg: AgentMessage) -> Result<(), SwarmError> {
        self.status = AgentStatus::Thinking;

        match msg.content {
            MessageContent::Heartbeat => {
                // 心跳响应
                self.status = AgentStatus::Idle;
            }
            MessageContent::Shutdown => {
                self.status = AgentStatus::Failed;
            }
            _ => {
                // 其他消息类型暂不处理
                self.status = AgentStatus::Idle;
            }
        }

        Ok(())
    }

    /// 生成市场信号（模拟）
    pub fn generate_signal(&self, symbol: &str) -> MarketSignal {
        MarketSignal {
            symbol: symbol.to_string(),
            signal_type: SignalType::Hold,
            confidence: 0.5,
            reasoning: "Insufficient data".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::agent::AgentId;

    #[test]
    fn test_market_agent_creation() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig::default();
        let agent = MarketAgent::new(id.clone(), config, rx, tx);

        assert_eq!(agent.id(), &id);
        assert_eq!(agent.role(), AgentRole::Market);
        assert_eq!(agent.status(), AgentStatus::Idle);
    }

    #[test]
    fn test_market_agent_symbols() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig {
            symbols: vec!["BTC-USDT".into(), "ETH-USDT".into()],
            signal_threshold: 0.8,
        };
        let agent = MarketAgent::new(id, config, rx, tx);

        assert_eq!(agent.symbols().len(), 2);
        assert!(agent.symbols().contains(&"BTC-USDT".to_string()));
    }

    #[test]
    fn test_market_agent_generate_signal() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig::default();
        let agent = MarketAgent::new(id, config, rx, tx);

        let signal = agent.generate_signal("BTC-USDT");
        assert_eq!(signal.symbol, "BTC-USDT");
        assert!(signal.confidence >= 0.0 && signal.confidence <= 1.0);
    }

    #[tokio::test]
    async fn test_market_agent_handle_heartbeat() {
        let (tx, rx) = mpsc::channel(10);
        let id = AgentId::from_string("market_0");
        let config = MarketAgentConfig::default();
        let mut agent = MarketAgent::new(id, config, rx, tx);

        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("orchestrator"),
            to: AgentId::from_string("market_0"),
            correlation_id: None,
            content: MessageContent::Heartbeat,
            timestamp: 1000,
        };

        agent.handle_message(msg).await.unwrap();
        assert_eq!(agent.status(), AgentStatus::Idle);
    }
}
