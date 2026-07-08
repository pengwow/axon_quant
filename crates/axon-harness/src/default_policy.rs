//! 默认裁决策略
//!
//! 基于 HarnessConfig 的默认裁决策略，包含：
//! - 熔断器检查
//! - 置信度检查
//! - 预算区间检查

use axon_core::harness_types::{AgentIntent, TaskContext};

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
use crate::policy::HarnessPolicy;
use crate::types::{
    Adjudication, BudgetState, BudgetZone, CompressionHint, HarnessConfig, ModelChoice,
};

/// 默认裁决策略
///
/// 基于 HarnessConfig 的默认裁决策略，包含：
/// - 熔断器检查：连续失败、日亏损、仓位超限、日交易次数
/// - 置信度检查：低于阈值则拒绝
/// - 预算区间检查：根据 Token 消耗比例选择裁决结果
pub struct DefaultPolicy {
    config: HarnessConfig,
    circuit_breaker: CircuitBreaker,
}

impl DefaultPolicy {
    /// 创建默认策略
    pub fn new(config: HarnessConfig) -> Self {
        let cb_config = CircuitBreakerConfig {
            max_consecutive_failures: 100, // 高阈值，主要靠 Token 预算
            cooldown_seconds: 60,
            max_daily_loss_pct: 100.0,
            max_position_pct: 100.0,
            max_daily_trades: 10000,
        };
        Self {
            config,
            circuit_breaker: CircuitBreaker::new(cb_config),
        }
    }

    /// 计算当前预算区间
    fn budget_zone(&self, tokens_used: u64) -> BudgetZone {
        let ratio = tokens_used as f64 / self.config.max_tokens as f64;
        if ratio >= 1.0 {
            BudgetZone::CircuitBreak
        } else if ratio >= self.config.red_zone_threshold {
            BudgetZone::Red
        } else if ratio >= self.config.yellow_zone_threshold {
            BudgetZone::Yellow
        } else {
            BudgetZone::Green
        }
    }
}

impl HarnessPolicy for DefaultPolicy {
    fn adjudicate(&self, intent: &AgentIntent, ctx: &TaskContext) -> Adjudication {
        // 1. 检查熔断器
        if self.circuit_breaker.is_open() {
            return Adjudication::CircuitBreak;
        }

        // 2. 检查置信度
        if intent.confidence < 0.3 {
            return Adjudication::Rejected("置信度过低".into());
        }

        // 3. 检查预算区间
        let zone = self.budget_zone(ctx.tokens_used);
        match zone {
            BudgetZone::CircuitBreak => Adjudication::CircuitBreak,
            BudgetZone::Red if intent.confidence < 0.8 => {
                Adjudication::NeedRevision("红区需要高置信度".into())
            }
            _ => Adjudication::Approved,
        }
    }

    fn can_proceed(&self, ctx: &TaskContext) -> bool {
        ctx.step < self.config.max_steps
            && ctx.tokens_used < self.config.max_tokens
            && !self.circuit_breaker.is_open()
    }

    fn select_model(&self, budget: &BudgetState) -> ModelChoice {
        match budget.zone {
            BudgetZone::Green => ModelChoice::FullPower,
            BudgetZone::Yellow => ModelChoice::Lightweight,
            _ => ModelChoice::Local,
        }
    }

    fn compression_hint(&self, budget: &BudgetState) -> CompressionHint {
        match budget.zone {
            BudgetZone::Green => CompressionHint::None,
            BudgetZone::Yellow => CompressionHint::SummarizeHistory,
            BudgetZone::Red => CompressionHint::StripNonEssentialTools,
            BudgetZone::CircuitBreak => CompressionHint::MinimalContext,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> HarnessConfig {
        HarnessConfig {
            max_steps: 50,
            max_tokens: 100_000,
            timeout_secs: 300,
            green_zone_threshold: 0.6,
            yellow_zone_threshold: 0.8,
            red_zone_threshold: 0.95,
        }
    }

    fn test_intent() -> AgentIntent {
        AgentIntent {
            action: "test".into(),
            tool: None,
            params: serde_json::Value::Null,
            confidence: 0.8,
            reasoning: "test".into(),
            estimated_tokens: 100,
        }
    }

    fn test_ctx() -> TaskContext {
        TaskContext {
            step: 1,
            tokens_used: 100,
            task_description: "test".into(),
            current_agent: "market".into(),
            started_at: 1000,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn test_approve_high_confidence() {
        let policy = DefaultPolicy::new(test_config());
        let intent = test_intent();
        let ctx = test_ctx();
        assert_eq!(policy.adjudicate(&intent, &ctx), Adjudication::Approved);
    }

    #[test]
    fn test_reject_low_confidence() {
        let policy = DefaultPolicy::new(test_config());
        let intent = AgentIntent {
            confidence: 0.2,
            ..test_intent()
        };
        let ctx = test_ctx();
        assert!(matches!(
            policy.adjudicate(&intent, &ctx),
            Adjudication::Rejected(_)
        ));
    }

    #[test]
    fn test_budget_zone_green() {
        let policy = DefaultPolicy::new(test_config());
        let ctx = TaskContext {
            tokens_used: 50_000, // 50%
            ..test_ctx()
        };
        let intent = test_intent();
        assert_eq!(policy.adjudicate(&intent, &ctx), Adjudication::Approved);
    }

    #[test]
    fn test_budget_zone_red() {
        let policy = DefaultPolicy::new(test_config());
        let ctx = TaskContext {
            tokens_used: 96_000, // 96%
            ..test_ctx()
        };
        let intent = AgentIntent {
            confidence: 0.7, // < 0.8
            ..test_intent()
        };
        assert!(matches!(
            policy.adjudicate(&intent, &ctx),
            Adjudication::NeedRevision(_)
        ));
    }

    #[test]
    fn test_can_proceed() {
        let policy = DefaultPolicy::new(test_config());
        let ctx = test_ctx();
        assert!(policy.can_proceed(&ctx));
    }

    #[test]
    fn test_cannot_proceed_max_steps() {
        let policy = DefaultPolicy::new(test_config());
        let ctx = TaskContext {
            step: 51,
            ..test_ctx()
        };
        assert!(!policy.can_proceed(&ctx));
    }
}
