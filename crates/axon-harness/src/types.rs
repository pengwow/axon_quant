//! Harness 层数据类型定义

use serde::{Deserialize, Serialize};

/// Harness 对 Agent 意图的裁决结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Adjudication {
    /// 批准执行
    Approved,
    /// 拒绝，附原因
    Rejected(String),
    /// 需要修改，附反馈
    NeedRevision(String),
    /// 熔断器触发，停止一切
    CircuitBreak,
}

/// Token 预算区间
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetZone {
    /// <60%: 满血运行
    Green,
    /// 60-80%: 上下文极简模式
    Yellow,
    /// 80-95%: 降级廉价模型
    Red,
    /// >=100%: 强制熔断
    CircuitBreak,
}

/// 工具门控结果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GateResult {
    /// 允许调用
    Allowed,
    /// 拒绝，附原因
    Denied(String),
    /// 需要人工审批
    NeedsApproval,
}

/// 模型选择
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelChoice {
    /// 强模型 (gpt-4o)
    FullPower,
    /// 轻量模型 (gpt-4o-mini)
    Lightweight,
    /// 本地模型（最低延迟）
    Local,
}

/// 上下文压缩提示
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionHint {
    /// 不需要压缩
    None,
    /// 压缩历史对话
    SummarizeHistory,
    /// 剥离非核心工具定义
    StripNonEssentialTools,
    /// 极简上下文
    MinimalContext,
}

/// Harness 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    /// 最大步数，默认 50
    pub max_steps: u32,
    /// 最大 Token，默认 100000
    pub max_tokens: u64,
    /// 超时秒数，默认 300
    pub timeout_secs: u64,
    /// 绿区阈值，默认 0.6
    pub green_zone_threshold: f64,
    /// 黄区阈值，默认 0.8
    pub yellow_zone_threshold: f64,
    /// 红区阈值，默认 0.95
    pub red_zone_threshold: f64,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            max_steps: 50,
            max_tokens: 100_000,
            timeout_secs: 300,
            green_zone_threshold: 0.6,
            yellow_zone_threshold: 0.8,
            red_zone_threshold: 0.95,
        }
    }
}

/// 预算状态快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetState {
    /// 总预算
    pub total_budget: u64,
    /// 已使用 Token
    pub tokens_used: u64,
    /// 当前区间
    pub zone: BudgetZone,
    /// 费用 (USD)
    pub cost_usd: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adjudication_serde() {
        let adj = Adjudication::Approved;
        let json = serde_json::to_string(&adj).unwrap();
        let back: Adjudication = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Adjudication::Approved);
    }

    #[test]
    fn test_harness_config_default() {
        let cfg = HarnessConfig::default();
        assert_eq!(cfg.max_steps, 50);
        assert_eq!(cfg.max_tokens, 100_000);
    }
}
