//! Harness 编排系统核心类型
//!
//! 定义 Agent 声明式意图、任务上下文、执行结果等贯穿 Harness 层的数据结构。

use serde::{Deserialize, Serialize};

/// Agent 的声明式意图
///
/// Agent 在 Act 阶段返回 Intent，不直接调用工具，由 Harness 裁决后决定是否执行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIntent {
    /// 意图描述，如 "buy BTC with 5% portfolio"
    pub action: String,
    /// 建议使用的工具名
    pub tool: Option<String>,
    /// 工具参数
    pub params: serde_json::Value,
    /// 置信度 0.0-1.0
    pub confidence: f64,
    /// 推理过程
    pub reasoning: String,
    /// 预估 Token 消耗
    pub estimated_tokens: u64,
}

/// 任务上下文
///
/// 记录任务执行过程中的状态信息，供 Harness 层做裁决依据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContext {
    /// 当前步数
    pub step: u32,
    /// 已消耗 Token
    pub tokens_used: u64,
    /// 任务描述
    pub task_description: String,
    /// 当前处理的 Agent
    pub current_agent: String,
    /// 开始时间 (Unix 秒)
    pub started_at: u64,
    /// 附加元数据
    pub metadata: serde_json::Value,
}

impl TaskContext {
    /// 推进一步：步数 +1，Token 消耗累加
    #[inline]
    pub fn advance(&mut self, tokens: u64) {
        self.step += 1;
        self.tokens_used += tokens;
    }
}

/// Harness 执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HarnessResult {
    /// 成功执行
    Success {
        /// Agent 意图
        intent: AgentIntent,
        /// 工具执行结果（JSON 字符串）
        tool_result: String,
        /// 循环迭代次数
        iterations: u32,
        /// Token 消耗
        tokens_used: u64,
    },
    /// 仅生成意图，未执行工具
    IntentOnly {
        /// Agent 意图
        intent: AgentIntent,
        /// 循环迭代次数
        iterations: u32,
    },
    /// 被拒绝
    Rejected {
        /// Agent 意图
        intent: AgentIntent,
        /// 拒绝原因
        reason: String,
    },
    /// 工具调用被拒绝
    ToolDenied {
        /// Agent 意图
        intent: AgentIntent,
        /// 拒绝原因
        reason: String,
    },
    /// 需要人工审批
    NeedsApproval {
        /// Agent 意图
        intent: AgentIntent,
    },
    /// 熔断器触发
    CircuitBreak,
    /// 达到最大迭代次数
    MaxIterationsReached,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_context_advance() {
        let mut ctx = TaskContext {
            step: 0,
            tokens_used: 0,
            task_description: "test".into(),
            current_agent: "market".into(),
            started_at: 1000,
            metadata: serde_json::Value::Null,
        };
        ctx.advance(500);
        assert_eq!(ctx.step, 1);
        assert_eq!(ctx.tokens_used, 500);
        ctx.advance(300);
        assert_eq!(ctx.step, 2);
        assert_eq!(ctx.tokens_used, 800);
    }

    #[test]
    fn test_agent_intent_serde() {
        let intent = AgentIntent {
            action: "buy BTC".into(),
            tool: Some("place_order".into()),
            params: serde_json::json!({"symbol": "BTC", "qty": 0.1}),
            confidence: 0.85,
            reasoning: "bullish signal".into(),
            estimated_tokens: 2000,
        };
        let json = serde_json::to_string(&intent).unwrap();
        let back: AgentIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.action, "buy BTC");
        assert!((back.confidence - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_harness_result_serde() {
        let result = HarnessResult::CircuitBreak;
        let json = serde_json::to_string(&result).unwrap();
        let back: HarnessResult = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, HarnessResult::CircuitBreak));
    }
}
