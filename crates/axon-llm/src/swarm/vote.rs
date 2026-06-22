//! 共识管理器 - 投票机制

use std::collections::HashMap;

use crate::swarm::agent::AgentId;
use crate::swarm::message::{VoteProposal, VoteResult, VoteType};

/// 投票响应
#[derive(Debug, Clone)]
pub struct VoteResponse {
    /// 提案 ID
    pub proposal_id: String,
    /// 投票者
    pub voter: AgentId,
    /// 是否赞成
    pub approved: bool,
    /// 原因
    pub reasoning: String,
    /// 置信度
    pub confidence: f64,
}

/// 法定人数规则
#[derive(Debug, Clone)]
pub enum QuorumRule {
    /// 简单多数（>50%）
    SimpleMajority,
    /// 全票通过
    Unanimous,
    /// 特定数量
    ExactCount(usize),
}

/// 共识管理器
#[derive(Default)]
pub struct ConsensusManager {
    /// 待处理提案
    pending_proposals: HashMap<String, VoteProposal>,
    /// 投票响应
    vote_responses: HashMap<String, Vec<VoteResponse>>,
    /// 法定人数规则
    quorum_rules: HashMap<VoteType, QuorumRule>,
}

impl ConsensusManager {
    /// 创建新的共识管理器
    pub fn new() -> Self {
        let mut quorum_rules = HashMap::new();
        quorum_rules.insert(VoteType::TradeDecision, QuorumRule::SimpleMajority);
        quorum_rules.insert(VoteType::EmergencyStop, QuorumRule::SimpleMajority);
        quorum_rules.insert(VoteType::StrategyAdjustment, QuorumRule::Unanimous);

        Self {
            pending_proposals: HashMap::new(),
            vote_responses: HashMap::new(),
            quorum_rules,
        }
    }

    /// 提交提案
    pub fn submit_proposal(&mut self, proposal: VoteProposal) {
        let proposal_id = proposal.proposal_id.clone();
        self.pending_proposals.insert(proposal_id, proposal);
    }

    /// 提交投票
    pub fn submit_vote(&mut self, response: VoteResponse) -> Option<VoteResult> {
        let proposal_id = response.proposal_id.clone();
        self.vote_responses
            .entry(proposal_id.clone())
            .or_default()
            .push(response);

        // 检查是否达到法定人数
        let votes = self.vote_responses.get(&proposal_id)?;
        let proposal = self.pending_proposals.get(&proposal_id)?;

        if self.check_quorum(proposal, votes) {
            Some(self.tally_votes(proposal_id))
        } else {
            None
        }
    }

    /// 检查法定人数
    fn check_quorum(&self, proposal: &VoteProposal, votes: &[VoteResponse]) -> bool {
        let rule = self
            .quorum_rules
            .get(&proposal.proposal_type)
            .unwrap_or(&QuorumRule::SimpleMajority);

        match rule {
            QuorumRule::SimpleMajority => votes.len() >= 2,
            QuorumRule::Unanimous => votes.len() >= 3,
            QuorumRule::ExactCount(n) => votes.len() >= *n,
        }
    }

    /// 统计投票
    fn tally_votes(&self, proposal_id: String) -> VoteResult {
        let votes = self.vote_responses.get(&proposal_id).unwrap();
        let approve = votes.iter().filter(|v| v.approved).count();
        let reject = votes.iter().filter(|v| !v.approved).count();

        VoteResult {
            proposal_id,
            passed: approve > reject,
            approve_count: approve,
            reject_count: reject,
            abstain_count: 0,
        }
    }

    /// 获取提案
    pub fn get_proposal(&self, proposal_id: &str) -> Option<&VoteProposal> {
        self.pending_proposals.get(proposal_id)
    }

    /// 获取投票响应
    pub fn get_votes(&self, proposal_id: &str) -> Option<&Vec<VoteResponse>> {
        self.vote_responses.get(proposal_id)
    }

    /// 清理已完成的提案
    pub fn cleanup(&mut self, proposal_id: &str) {
        self.pending_proposals.remove(proposal_id);
        self.vote_responses.remove(proposal_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::message::VoteType;

    #[test]
    fn test_consensus_manager_creation() {
        let manager = ConsensusManager::new();
        assert!(manager.pending_proposals.is_empty());
        assert!(manager.vote_responses.is_empty());
    }

    #[test]
    fn test_submit_proposal() {
        let mut manager = ConsensusManager::new();
        let proposal = VoteProposal {
            proposal_id: "vote_001".into(),
            proposal_type: VoteType::TradeDecision,
            content: "Buy BTC-USDT".into(),
            deadline_ms: 5000,
        };

        manager.submit_proposal(proposal);
        assert!(manager.get_proposal("vote_001").is_some());
    }

    #[test]
    fn test_submit_vote_not_passed() {
        let mut manager = ConsensusManager::new();
        let proposal = VoteProposal {
            proposal_id: "vote_001".into(),
            proposal_type: VoteType::TradeDecision,
            content: "Buy BTC-USDT".into(),
            deadline_ms: 5000,
        };
        manager.submit_proposal(proposal);

        // 只有 1 票，未达到法定人数
        let response = VoteResponse {
            proposal_id: "vote_001".into(),
            voter: AgentId::from_string("risk_0"),
            approved: true,
            reasoning: "Looks good".into(),
            confidence: 0.8,
        };

        let result = manager.submit_vote(response);
        assert!(result.is_none());
    }

    #[test]
    fn test_submit_vote_passed() {
        let mut manager = ConsensusManager::new();
        let proposal = VoteProposal {
            proposal_id: "vote_001".into(),
            proposal_type: VoteType::TradeDecision,
            content: "Buy BTC-USDT".into(),
            deadline_ms: 5000,
        };
        manager.submit_proposal(proposal);

        // 2 票赞成
        manager.submit_vote(VoteResponse {
            proposal_id: "vote_001".into(),
            voter: AgentId::from_string("risk_0"),
            approved: true,
            reasoning: "Approved".into(),
            confidence: 0.9,
        });

        let result = manager.submit_vote(VoteResponse {
            proposal_id: "vote_001".into(),
            voter: AgentId::from_string("execution_0"),
            approved: true,
            reasoning: "Feasible".into(),
            confidence: 0.85,
        });

        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.passed);
        assert_eq!(result.approve_count, 2);
        assert_eq!(result.reject_count, 0);
    }

    #[test]
    fn test_submit_vote_rejected() {
        let mut manager = ConsensusManager::new();
        let proposal = VoteProposal {
            proposal_id: "vote_001".into(),
            proposal_type: VoteType::TradeDecision,
            content: "Buy BTC-USDT".into(),
            deadline_ms: 5000,
        };
        manager.submit_proposal(proposal);

        // 1 票赞成，2 票反对
        manager.submit_vote(VoteResponse {
            proposal_id: "vote_001".into(),
            voter: AgentId::from_string("risk_0"),
            approved: true,
            reasoning: "Approved".into(),
            confidence: 0.9,
        });
        manager.submit_vote(VoteResponse {
            proposal_id: "vote_001".into(),
            voter: AgentId::from_string("execution_0"),
            approved: false,
            reasoning: "Too risky".into(),
            confidence: 0.3,
        });

        let result = manager.submit_vote(VoteResponse {
            proposal_id: "vote_001".into(),
            voter: AgentId::from_string("market_0"),
            approved: false,
            reasoning: "Bad timing".into(),
            confidence: 0.4,
        });

        assert!(result.is_some());
        let result = result.unwrap();
        assert!(!result.passed);
        assert_eq!(result.approve_count, 1);
        assert_eq!(result.reject_count, 2);
    }

    #[test]
    fn test_cleanup() {
        let mut manager = ConsensusManager::new();
        let proposal = VoteProposal {
            proposal_id: "vote_001".into(),
            proposal_type: VoteType::TradeDecision,
            content: "Buy BTC-USDT".into(),
            deadline_ms: 5000,
        };
        manager.submit_proposal(proposal);

        manager.cleanup("vote_001");
        assert!(manager.get_proposal("vote_001").is_none());
    }
}
