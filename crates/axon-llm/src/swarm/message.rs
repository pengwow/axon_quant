//! Agent 间消息协议

use serde::{Deserialize, Serialize};

use super::agent::AgentId;

/// 消息 ID
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct MessageId(pub String);

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageId {
    /// 创建新的消息 ID
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(format!("msg_{}", id))
    }
}

/// Agent 间消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    /// 消息 ID
    pub id: MessageId,
    /// 发送者
    pub from: AgentId,
    /// 接收者
    pub to: AgentId,
    /// 关联 ID（用于投票）
    pub correlation_id: Option<String>,
    /// 消息内容
    pub content: MessageContent,
    /// 时间戳
    pub timestamp: i64,
}

/// 消息内容
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageContent {
    /// 市场分析
    MarketAnalysis(MarketSignal),
    /// 风险评估
    RiskAssessment(RiskSignal),
    /// 执行请求
    ExecutionRequest(TradeOrder),
    /// 执行结果
    ExecutionResult(TradeResult),
    /// 投票请求
    VoteRequest(VoteProposal),
    /// 投票响应
    VoteResponse(VoteResult),
    /// 心跳
    Heartbeat,
    /// 关闭
    Shutdown,
}

/// 市场信号
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSignal {
    /// 交易对
    pub symbol: String,
    /// 信号类型
    pub signal_type: SignalType,
    /// 置信度
    pub confidence: f64,
    /// 推理过程
    pub reasoning: String,
}

/// 信号类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalType {
    /// 买入
    Buy,
    /// 卖出
    Sell,
    /// 持有
    Hold,
}

impl std::fmt::Display for SignalType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Buy => write!(f, "Buy"),
            Self::Sell => write!(f, "Sell"),
            Self::Hold => write!(f, "Hold"),
        }
    }
}

/// 风险信号
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskSignal {
    /// 交易对
    pub symbol: String,
    /// 是否批准
    pub approved: bool,
    /// 风险分数
    pub risk_score: f64,
    /// 违规原因
    pub violations: Vec<String>,
}

/// 交易订单
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeOrder {
    /// 交易对
    pub symbol: String,
    /// 方向
    pub side: OrderSide,
    /// 数量
    pub quantity: f64,
    /// 订单类型
    pub order_type: String,
    /// 价格
    pub price: Option<f64>,
    /// 原因
    pub reason: String,
}

/// 订单方向
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderSide {
    /// 买入
    Buy,
    /// 卖出
    Sell,
}

/// 交易结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeResult {
    /// 订单 ID
    pub order_id: String,
    /// 是否成功
    pub success: bool,
    /// 错误信息
    pub error: Option<String>,
}

/// 投票提案
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteProposal {
    /// 提案 ID
    pub proposal_id: String,
    /// 提案类型
    pub proposal_type: VoteType,
    /// 提案内容
    pub content: String,
    /// 截止时间（毫秒）
    pub deadline_ms: i64,
}

/// 投票类型
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VoteType {
    /// 交易决策
    TradeDecision,
    /// 紧急止损
    EmergencyStop,
    /// 策略调整
    StrategyAdjustment,
}

/// 投票结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteResult {
    /// 提案 ID
    pub proposal_id: String,
    /// 是否通过
    pub passed: bool,
    /// 赞成票数
    pub approve_count: usize,
    /// 反对票数
    pub reject_count: usize,
    /// 弃权票数
    pub abstain_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_id_creation() {
        let id1 = MessageId::new();
        let id2 = MessageId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_agent_message_creation() {
        let msg = AgentMessage {
            id: MessageId::new(),
            from: AgentId::from_string("market_0"),
            to: AgentId::from_string("risk_0"),
            correlation_id: None,
            content: MessageContent::Heartbeat,
            timestamp: 1000,
        };
        assert_eq!(msg.from.as_str(), "market_0");
        assert_eq!(msg.to.as_str(), "risk_0");
    }

    #[test]
    fn test_market_signal_creation() {
        let signal = MarketSignal {
            symbol: "BTC-USDT".into(),
            signal_type: SignalType::Buy,
            confidence: 0.85,
            reasoning: "Strong bullish momentum".into(),
        };
        assert_eq!(signal.symbol, "BTC-USDT");
        assert!(signal.confidence > 0.8);
    }

    #[test]
    fn test_vote_proposal_creation() {
        let proposal = VoteProposal {
            proposal_id: "vote_001".into(),
            proposal_type: VoteType::TradeDecision,
            content: "Buy BTC-USDT".into(),
            deadline_ms: 5000,
        };
        assert_eq!(proposal.proposal_id, "vote_001");
    }

    #[test]
    fn test_vote_result_passed() {
        let result = VoteResult {
            proposal_id: "vote_001".into(),
            passed: true,
            approve_count: 2,
            reject_count: 1,
            abstain_count: 0,
        };
        assert!(result.passed);
        assert!(result.approve_count > result.reject_count);
    }
}
