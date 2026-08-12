//! 投票共识 Multi-Agent 决策层（0.11.0）
//!
//! N 个独立 ReAct trader → ensemble 投票聚合 → risk 一票否决 → 最终 Action。
//!
//! 与 `swarm::orchestrator`（通用 agent 编排）不同，本模块专注于
//! "多 trader 投票共识" 这一简洁模式，面向 bar-by-bar 回测/实盘场景。

use serde::{Deserialize, Serialize};

use crate::meter::TokenReport;

// ═══════════════════════════════════════════════════════════
// 2.1 类型定义
// ═══════════════════════════════════════════════════════════

/// Trader 动作类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TraderAction {
    /// 买入/做多
    Buy,
    /// 卖出/做空
    Sell,
    /// 持有/不操作
    Hold,
}

impl std::fmt::Display for TraderAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Buy => write!(f, "Buy"),
            Self::Sell => write!(f, "Sell"),
            Self::Hold => write!(f, "Hold"),
        }
    }
}

/// 单个 trader 的投票
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentVote {
    /// Agent 标识
    pub agent_id: String,
    /// 决策动作
    pub action: TraderAction,
    /// 置信度 [0.0, 1.0]
    pub confidence: f64,
    /// 推理摘要
    pub reasoning: String,
}

/// 聚合投票结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedVote {
    /// 胜出动作
    pub action: TraderAction,
    /// 聚合得分
    pub score: f64,
    /// 使用的策略名
    pub strategy: String,
}

/// Risk 审核结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskVerdict {
    /// 是否通过
    pub approved: bool,
    /// 否决原因（通过时为 None）
    pub reason: Option<String>,
}

/// 完整决策输出
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusDecision {
    /// 最终动作（被否决时为 Hold）
    pub final_action: TraderAction,
    /// 最终置信度
    pub final_confidence: f64,
    /// 各 trader 原始投票
    pub votes: Vec<AgentVote>,
    /// 聚合结果
    pub aggregated: AggregatedVote,
    /// Risk 审核
    pub risk_verdict: RiskVerdict,
    /// 本 bar token 消耗
    #[serde(default)]
    pub token_usage: Option<TokenUsageSnapshot>,
}

/// Token 使用快照（可序列化版本）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsageSnapshot {
    /// input tokens
    pub input_tokens: u64,
    /// output tokens
    pub output_tokens: u64,
    /// 估算费用
    pub estimated_cost_usd: f64,
}

impl From<&TokenReport> for TokenUsageSnapshot {
    fn from(r: &TokenReport) -> Self {
        Self {
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            estimated_cost_usd: r.estimated_cost_usd,
        }
    }
}

// ═══════════════════════════════════════════════════════════
// 2.2 + 2.3 投票策略
// ═══════════════════════════════════════════════════════════

/// 投票聚合策略 trait
pub trait VotingStrategy: Send + Sync {
    /// 聚合多个投票为单一结果
    fn aggregate(&self, votes: &[AgentVote]) -> AggregatedVote;
    /// 策略名
    fn name(&self) -> &str;
}

/// 加权多数投票：confidence 加权，超阈值胜出
#[derive(Debug, Clone)]
pub struct WeightedMajorityVote {
    /// 胜出阈值（加权得分 > threshold 才非 Hold）
    pub threshold: f64,
}

impl Default for WeightedMajorityVote {
    fn default() -> Self {
        Self { threshold: 0.5 }
    }
}

impl VotingStrategy for WeightedMajorityVote {
    fn aggregate(&self, votes: &[AgentVote]) -> AggregatedVote {
        if votes.is_empty() {
            return AggregatedVote {
                action: TraderAction::Hold,
                score: 0.0,
                strategy: self.name().to_string(),
            };
        }
        // 按 action 累加 confidence
        let mut buy_score = 0.0f64;
        let mut sell_score = 0.0f64;
        let mut hold_score = 0.0f64;
        for v in votes {
            match v.action {
                TraderAction::Buy => buy_score += v.confidence,
                TraderAction::Sell => sell_score += v.confidence,
                TraderAction::Hold => hold_score += v.confidence.max(0.1), // Hold 给最低分
            }
        }
        // 归一化
        let total = buy_score + sell_score + hold_score;
        let (action, raw_score) = if buy_score >= sell_score && buy_score >= hold_score {
            (TraderAction::Buy, buy_score)
        } else if sell_score >= buy_score && sell_score >= hold_score {
            (TraderAction::Sell, sell_score)
        } else {
            (TraderAction::Hold, hold_score)
        };
        let normalized = if total > 0.0 { raw_score / total } else { 0.0 };
        // 低于阈值 → Hold
        let final_action = if normalized < self.threshold {
            TraderAction::Hold
        } else {
            action
        };
        AggregatedVote {
            action: final_action,
            score: normalized,
            strategy: self.name().to_string(),
        }
    }

    fn name(&self) -> &str {
        "weighted_majority"
    }
}

/// 全票一致投票：所有 trader 同一方向才通过
#[derive(Debug, Clone, Default)]
pub struct UnanimousVote;

impl VotingStrategy for UnanimousVote {
    fn aggregate(&self, votes: &[AgentVote]) -> AggregatedVote {
        if votes.is_empty() {
            return AggregatedVote {
                action: TraderAction::Hold,
                score: 0.0,
                strategy: self.name().to_string(),
            };
        }
        let first = votes[0].action;
        let all_same = votes.iter().all(|v| v.action == first);
        if all_same && first != TraderAction::Hold {
            let avg_conf = votes.iter().map(|v| v.confidence).sum::<f64>() / votes.len() as f64;
            AggregatedVote {
                action: first,
                score: avg_conf,
                strategy: self.name().to_string(),
            }
        } else {
            AggregatedVote {
                action: TraderAction::Hold,
                score: 0.0,
                strategy: self.name().to_string(),
            }
        }
    }

    fn name(&self) -> &str {
        "unanimous"
    }
}

// ═══════════════════════════════════════════════════════════
// 2.4 RiskAgent 纯规则引擎
// ═══════════════════════════════════════════════════════════

/// Risk 审核上下文
#[derive(Debug, Clone, Default)]
pub struct RiskContext {
    /// 当前持仓（正=多，负=空）
    pub current_position: f64,
    /// 连续亏损次数
    pub consecutive_losses: u32,
    /// 当前回撤（正数，如 0.15 = 15%）
    pub drawdown: f64,
}

/// 纯规则 Risk Agent（不走 LLM，快速确定性审核）
#[derive(Debug, Clone)]
pub struct ConsensusRiskAgent {
    /// 单方向最大仓位
    pub max_position: f64,
    /// 连续亏损 N 次后否决开仓
    pub max_consecutive_loss: u32,
    /// 回撤超阈值否决
    pub max_drawdown: f64,
}

impl Default for ConsensusRiskAgent {
    fn default() -> Self {
        Self {
            max_position: 1.0,
            max_consecutive_loss: 5,
            max_drawdown: 0.3,
        }
    }
}

impl ConsensusRiskAgent {
    /// 审核聚合结果，返回通过/否决
    pub fn review(&self, aggregated: &AggregatedVote, ctx: &RiskContext) -> RiskVerdict {
        // Hold 永远通过
        if aggregated.action == TraderAction::Hold {
            return RiskVerdict {
                approved: true,
                reason: None,
            };
        }
        // 回撤检查
        if ctx.drawdown > self.max_drawdown {
            return RiskVerdict {
                approved: false,
                reason: Some(format!(
                    "drawdown {:.1}% exceeds limit {:.1}%",
                    ctx.drawdown * 100.0,
                    self.max_drawdown * 100.0
                )),
            };
        }
        // 连续亏损检查
        if ctx.consecutive_losses >= self.max_consecutive_loss {
            return RiskVerdict {
                approved: false,
                reason: Some(format!(
                    "consecutive losses {} >= limit {}",
                    ctx.consecutive_losses, self.max_consecutive_loss
                )),
            };
        }
        // 仓位检查
        let new_pos = match aggregated.action {
            TraderAction::Buy => ctx.current_position + aggregated.score,
            TraderAction::Sell => ctx.current_position - aggregated.score,
            TraderAction::Hold => ctx.current_position,
        };
        if new_pos.abs() > self.max_position {
            return RiskVerdict {
                approved: false,
                reason: Some(format!(
                    "position {:.3} would exceed max {:.3}",
                    new_pos, self.max_position
                )),
            };
        }
        RiskVerdict {
            approved: true,
            reason: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════
// 2.5 + 2.6 + 2.7 VotingOrchestrator
// ═══════════════════════════════════════════════════════════

/// Trader 回调 trait：输入 bar dict，输出投票
///
/// 实现者可以是 ReActAgent wrapper 或简单规则策略。
pub trait TraderFn: Send + Sync {
    /// 给定 bar 数据，返回投票
    fn decide(&self, bar: &serde_json::Value) -> AgentVote;
    /// Agent ID
    fn id(&self) -> &str;
}

/// 投票共识编排器
///
/// 管理 N 个 trader + 1 个 risk agent + 投票策略。
/// 每 bar 调用 `on_bar` 完成完整决策流程。
pub struct VotingOrchestrator {
    traders: Vec<Box<dyn TraderFn>>,
    risk_agent: ConsensusRiskAgent,
    voting: Box<dyn VotingStrategy>,
    /// 当前 risk 上下文（外部更新）
    risk_ctx: RiskContext,
}

impl VotingOrchestrator {
    /// 构造编排器
    pub fn new(
        traders: Vec<Box<dyn TraderFn>>,
        risk_agent: ConsensusRiskAgent,
        voting: Box<dyn VotingStrategy>,
    ) -> Self {
        Self {
            traders,
            risk_agent,
            voting,
            risk_ctx: RiskContext::default(),
        }
    }

    /// 更新 risk 上下文（每 bar 结束后由调用方更新）
    pub fn update_risk_context(&mut self, ctx: RiskContext) {
        self.risk_ctx = ctx;
    }

    /// 获取当前 risk 上下文
    pub fn risk_context(&self) -> &RiskContext {
        &self.risk_ctx
    }

    /// Trader 数量
    pub fn trader_count(&self) -> usize {
        self.traders.len()
    }

    /// 核心决策循环：分发 bar → 收集投票 → 聚合 → risk 审核
    ///
    /// ponytail: 0.11.0 顺序执行 trader；升级路径：rayon par_iter 并行
    pub fn on_bar(&self, bar: &serde_json::Value) -> ConsensusDecision {
        // 1. 收集投票（顺序执行）
        let votes: Vec<AgentVote> = self.traders.iter().map(|t| t.decide(bar)).collect();

        // 2. 聚合
        let aggregated = self.voting.aggregate(&votes);

        // 3. Risk 审核
        let risk_verdict = self.risk_agent.review(&aggregated, &self.risk_ctx);

        // 4. 最终决策
        let (final_action, final_confidence) = if risk_verdict.approved {
            (aggregated.action, aggregated.score)
        } else {
            (TraderAction::Hold, 0.0)
        };

        ConsensusDecision {
            final_action,
            final_confidence,
            votes,
            aggregated,
            risk_verdict,
            token_usage: None, // 由外部 TokenMeter 填充
        }
    }
}

// ═══════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ─── 2.3 VotingStrategy 测试 ───

    #[test]
    fn weighted_majority_all_buy() {
        let votes = vec![
            AgentVote {
                agent_id: "a".into(),
                action: TraderAction::Buy,
                confidence: 0.8,
                reasoning: "".into(),
            },
            AgentVote {
                agent_id: "b".into(),
                action: TraderAction::Buy,
                confidence: 0.6,
                reasoning: "".into(),
            },
            AgentVote {
                agent_id: "c".into(),
                action: TraderAction::Buy,
                confidence: 0.7,
                reasoning: "".into(),
            },
        ];
        let strategy = WeightedMajorityVote::default();
        let result = strategy.aggregate(&votes);
        assert_eq!(result.action, TraderAction::Buy);
        assert!(result.score > 0.5);
    }

    #[test]
    fn weighted_majority_split_vote() {
        let votes = vec![
            AgentVote {
                agent_id: "a".into(),
                action: TraderAction::Buy,
                confidence: 0.9,
                reasoning: "".into(),
            },
            AgentVote {
                agent_id: "b".into(),
                action: TraderAction::Sell,
                confidence: 0.4,
                reasoning: "".into(),
            },
            AgentVote {
                agent_id: "c".into(),
                action: TraderAction::Hold,
                confidence: 0.0,
                reasoning: "".into(),
            },
        ];
        let strategy = WeightedMajorityVote { threshold: 0.5 };
        let result = strategy.aggregate(&votes);
        // Buy=0.9, Sell=0.4, Hold=0.1 → Buy wins, normalized=0.9/1.4=0.64 > 0.5
        assert_eq!(result.action, TraderAction::Buy);
    }

    #[test]
    fn weighted_majority_below_threshold_becomes_hold() {
        let votes = vec![
            AgentVote {
                agent_id: "a".into(),
                action: TraderAction::Buy,
                confidence: 0.3,
                reasoning: "".into(),
            },
            AgentVote {
                agent_id: "b".into(),
                action: TraderAction::Sell,
                confidence: 0.3,
                reasoning: "".into(),
            },
            AgentVote {
                agent_id: "c".into(),
                action: TraderAction::Hold,
                confidence: 0.0,
                reasoning: "".into(),
            },
        ];
        let strategy = WeightedMajorityVote { threshold: 0.5 };
        let result = strategy.aggregate(&votes);
        // Buy=0.3, Sell=0.3, Hold=0.1 → Buy/Sell tie, Buy wins (>=), normalized=0.3/0.7=0.43 < 0.5
        assert_eq!(result.action, TraderAction::Hold);
    }

    #[test]
    fn unanimous_all_same() {
        let votes = vec![
            AgentVote {
                agent_id: "a".into(),
                action: TraderAction::Sell,
                confidence: 0.7,
                reasoning: "".into(),
            },
            AgentVote {
                agent_id: "b".into(),
                action: TraderAction::Sell,
                confidence: 0.8,
                reasoning: "".into(),
            },
        ];
        let strategy = UnanimousVote;
        let result = strategy.aggregate(&votes);
        assert_eq!(result.action, TraderAction::Sell);
        assert!((result.score - 0.75).abs() < 1e-6);
    }

    #[test]
    fn unanimous_disagreement_becomes_hold() {
        let votes = vec![
            AgentVote {
                agent_id: "a".into(),
                action: TraderAction::Buy,
                confidence: 0.9,
                reasoning: "".into(),
            },
            AgentVote {
                agent_id: "b".into(),
                action: TraderAction::Sell,
                confidence: 0.8,
                reasoning: "".into(),
            },
        ];
        let strategy = UnanimousVote;
        let result = strategy.aggregate(&votes);
        assert_eq!(result.action, TraderAction::Hold);
    }

    #[test]
    fn empty_votes_hold() {
        let strategy = WeightedMajorityVote::default();
        let result = strategy.aggregate(&[]);
        assert_eq!(result.action, TraderAction::Hold);
    }

    // ─── 2.4 RiskAgent 测试 ───

    #[test]
    fn risk_approves_normal() {
        let agent = ConsensusRiskAgent::default();
        let agg = AggregatedVote {
            action: TraderAction::Buy,
            score: 0.7,
            strategy: "test".into(),
        };
        let ctx = RiskContext::default();
        let verdict = agent.review(&agg, &ctx);
        assert!(verdict.approved);
    }

    #[test]
    fn risk_veto_drawdown() {
        let agent = ConsensusRiskAgent {
            max_drawdown: 0.2,
            ..Default::default()
        };
        let agg = AggregatedVote {
            action: TraderAction::Buy,
            score: 0.7,
            strategy: "test".into(),
        };
        let ctx = RiskContext {
            drawdown: 0.25,
            ..Default::default()
        };
        let verdict = agent.review(&agg, &ctx);
        assert!(!verdict.approved);
        assert!(verdict.reason.unwrap().contains("drawdown"));
    }

    #[test]
    fn risk_veto_consecutive_loss() {
        let agent = ConsensusRiskAgent {
            max_consecutive_loss: 3,
            ..Default::default()
        };
        let agg = AggregatedVote {
            action: TraderAction::Buy,
            score: 0.7,
            strategy: "test".into(),
        };
        let ctx = RiskContext {
            consecutive_losses: 3,
            ..Default::default()
        };
        let verdict = agent.review(&agg, &ctx);
        assert!(!verdict.approved);
        assert!(verdict.reason.unwrap().contains("consecutive"));
    }

    #[test]
    fn risk_veto_position_limit() {
        let agent = ConsensusRiskAgent {
            max_position: 0.5,
            ..Default::default()
        };
        let agg = AggregatedVote {
            action: TraderAction::Buy,
            score: 0.7,
            strategy: "test".into(),
        };
        let ctx = RiskContext {
            current_position: 0.0,
            ..Default::default()
        };
        // new_pos = 0.0 + 0.7 = 0.7 > 0.5
        let verdict = agent.review(&agg, &ctx);
        assert!(!verdict.approved);
        assert!(verdict.reason.unwrap().contains("position"));
    }

    #[test]
    fn risk_hold_always_approved() {
        let agent = ConsensusRiskAgent {
            max_drawdown: 0.01,
            ..Default::default()
        };
        let agg = AggregatedVote {
            action: TraderAction::Hold,
            score: 0.0,
            strategy: "test".into(),
        };
        let ctx = RiskContext {
            drawdown: 0.5,
            ..Default::default()
        };
        let verdict = agent.review(&agg, &ctx);
        assert!(verdict.approved);
    }

    // ─── 2.5 VotingOrchestrator 测试 ───

    struct MockTrader {
        id: String,
        action: TraderAction,
        confidence: f64,
    }

    impl TraderFn for MockTrader {
        fn decide(&self, _bar: &serde_json::Value) -> AgentVote {
            AgentVote {
                agent_id: self.id.clone(),
                action: self.action,
                confidence: self.confidence,
                reasoning: format!("mock {}", self.id),
            }
        }
        fn id(&self) -> &str {
            &self.id
        }
    }

    #[test]
    fn orchestrator_full_flow_approved() {
        let traders: Vec<Box<dyn TraderFn>> = vec![
            Box::new(MockTrader {
                id: "t1".into(),
                action: TraderAction::Buy,
                confidence: 0.8,
            }),
            Box::new(MockTrader {
                id: "t2".into(),
                action: TraderAction::Buy,
                confidence: 0.7,
            }),
            Box::new(MockTrader {
                id: "t3".into(),
                action: TraderAction::Hold,
                confidence: 0.0,
            }),
        ];
        let orch = VotingOrchestrator::new(
            traders,
            ConsensusRiskAgent::default(),
            Box::new(WeightedMajorityVote::default()),
        );
        let bar = serde_json::json!({"close": 67000.0});
        let decision = orch.on_bar(&bar);

        assert_eq!(decision.final_action, TraderAction::Buy);
        assert!(decision.risk_verdict.approved);
        assert_eq!(decision.votes.len(), 3);
        assert_eq!(decision.aggregated.strategy, "weighted_majority");
    }

    #[test]
    fn orchestrator_veto_becomes_hold() {
        let traders: Vec<Box<dyn TraderFn>> = vec![
            Box::new(MockTrader {
                id: "t1".into(),
                action: TraderAction::Buy,
                confidence: 0.9,
            }),
            Box::new(MockTrader {
                id: "t2".into(),
                action: TraderAction::Buy,
                confidence: 0.9,
            }),
        ];
        let risk = ConsensusRiskAgent {
            max_position: 0.5,
            ..Default::default()
        };
        let orch =
            VotingOrchestrator::new(traders, risk, Box::new(WeightedMajorityVote::default()));
        let bar = serde_json::json!({"close": 67000.0});
        let decision = orch.on_bar(&bar);

        // Aggregated Buy score > 0.5 → position would exceed → veto → Hold
        assert_eq!(decision.final_action, TraderAction::Hold);
        assert!(!decision.risk_verdict.approved);
    }

    #[test]
    fn orchestrator_trader_count() {
        let traders: Vec<Box<dyn TraderFn>> = vec![Box::new(MockTrader {
            id: "t1".into(),
            action: TraderAction::Buy,
            confidence: 0.5,
        })];
        let orch = VotingOrchestrator::new(
            traders,
            ConsensusRiskAgent::default(),
            Box::new(UnanimousVote),
        );
        assert_eq!(orch.trader_count(), 1);
    }

    /// 2.12 e2e: 3 MockTrader × 50 bar，验证 decision 序列完整 + 性能 < 100ms
    #[test]
    fn e2e_50bar_performance() {
        let traders: Vec<Box<dyn TraderFn>> = vec![
            Box::new(MockTrader {
                id: "t1".into(),
                action: TraderAction::Buy,
                confidence: 0.8,
            }),
            Box::new(MockTrader {
                id: "t2".into(),
                action: TraderAction::Buy,
                confidence: 0.6,
            }),
            Box::new(MockTrader {
                id: "t3".into(),
                action: TraderAction::Hold,
                confidence: 0.0,
            }),
        ];
        let orch = VotingOrchestrator::new(
            traders,
            ConsensusRiskAgent::default(),
            Box::new(WeightedMajorityVote::default()),
        );

        let start = std::time::Instant::now();
        let mut decisions = Vec::with_capacity(50);
        for i in 0..50 {
            let bar = serde_json::json!({"close": 67000.0 + i as f64, "volume": 100.0});
            decisions.push(orch.on_bar(&bar));
        }
        let elapsed = start.elapsed();

        // 性能 gate: < 100ms
        assert!(elapsed.as_millis() < 100, "50 bar took {:?}", elapsed);

        // 完整性
        assert_eq!(decisions.len(), 50);
        for d in &decisions {
            assert_eq!(d.votes.len(), 3);
            assert_eq!(d.final_action, TraderAction::Buy);
            assert!(d.risk_verdict.approved);
            assert_eq!(d.aggregated.strategy, "weighted_majority");
        }

        // JSON 序列化验证
        let json = serde_json::to_string(&decisions[0]).unwrap();
        assert!(json.contains("final_action"));
        assert!(json.contains("votes"));
    }
}
