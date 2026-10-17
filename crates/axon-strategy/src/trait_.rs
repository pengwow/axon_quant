//! 统一策略 trait
//!
//! 回测和实盘共用同一个 `decide()` 入口——Strategy 不感知自己运行在哪个上下文，
//! BacktestRunner 和 EventDrivenLoop 的消费模式完全相同。

use std::collections::BTreeMap;

use async_trait::async_trait;
use axon_core::market::bar::Bar;
use axon_llm::decision::Action;

/// 策略运行时上下文（回测/实盘通用）
///
/// 把 bar window、features、timestamp 等一次性打包传入，避免多次 Option 嵌套解引用。
pub struct BarContext<'a> {
    pub symbol: &'a str,
    pub bar_index: u64,
    pub timestamp_ns: u64,
    /// 当前 bar（OHLCV），Python 策略适配器需要完整 bar 数据
    pub bar: &'a Bar,
    /// 预计算的 state（features + window），由 BacktestRunner 负责构造
    pub state: &'a serde_json::Value,
}

/// 策略统一错误
#[derive(Debug, thiserror::Error)]
pub enum StrategyError {
    /// 内部决策后端错误（SDM / LLM / Harness 任一失败）
    #[error("decision backend: {0}")]
    Backend(String),

    /// Harness 治理层拒绝
    #[error("harness rejected: {reason}")]
    HarnessRejected { reason: String },

    /// Action 解析或状态构造失败
    #[error("strategy logic error: {0}")]
    Logic(String),
}

/// 统一策略 trait
///
/// ponytail: 故意不暴露同步方法，回测和实盘都走 async 管道，
/// 底层 DecisionBackend 本来就是 async_trait，保持一致减少胶水层。
#[async_trait]
pub trait Strategy: Send + Sync {
    /// 策略名（用于日志/多策略编排时的身份标识）
    fn name(&self) -> &str;

    /// 交易标的（用于 BacktestRunner 路由到对应的 EventQueue / PositionState）
    fn symbol(&self) -> &str;

    /// 核心入口：给 bar window 的 state，返回决策 Action
    async fn decide(&self, ctx: &BarContext<'_>) -> Result<StrategyDecision, StrategyError>;
}

/// 一次策略决策的完整出口
///
/// 比 Action 多带 decision_source、confidence、sub_signals——回测报告
/// 和 Harness 治理层都需要这些归因数据。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StrategyDecision {
    /// 最终动作
    pub action: Action,
    /// 置信度（SDM 校准值；规则策略为 0.0）
    pub confidence: f64,
    /// 目标仓位（-1.0 ~ 1.0；None 时由 BacktestRunner 按 action 推导：Buy→1.0, Sell→-1.0, Hold→0.0）
    pub target_position: Option<f64>,
    /// 决策来源（sdm_direct / fallback / rule）
    pub source: String,
    /// 买入/卖出/观望三键概率分布（规则策略为 None）
    pub probabilities: Option<BTreeMap<String, f64>>,
    /// 子信号（volatility_regime / drawdown_risk 等，纯归因，不参与门控）
    pub sub_signals: BTreeMap<String, f64>,
    /// 决策触发的原因（fallback_reason / harness_reject_reason 等）
    pub reason: Option<String>,
}

impl StrategyDecision {
    pub fn from_sdm(cascade: axon_llm::decision::CascadeDecision) -> Self {
        Self {
            action: cascade.action,
            confidence: cascade.confidence,
            target_position: None,
            source: match cascade.source {
                axon_llm::decision::DecisionSource::SdmDirect => "sdm_direct",
                axon_llm::decision::DecisionSource::Fallback => "fallback",
            }
            .into(),
            probabilities: Some(cascade.probabilities),
            sub_signals: cascade.sub_signals,
            reason: cascade.fallback_reason,
        }
    }

    pub fn from_rule(action: Action) -> Self {
        Self {
            action,
            confidence: 0.0,
            target_position: None,
            source: "rule".into(),
            probabilities: None,
            sub_signals: BTreeMap::new(),
            reason: None,
        }
    }

    /// 设置置信度（builder 风格）
    pub fn with_confidence(mut self, c: f64) -> Self {
        self.confidence = c;
        self
    }

    /// 设置目标仓位（builder 风格）
    pub fn with_target_position(mut self, t: f64) -> Self {
        self.target_position = Some(t);
        self
    }
}
