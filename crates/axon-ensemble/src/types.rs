//! 集成相关核心类型
//!
//! 定义 `Action` / `Observation` / `EnsembleAction` 等基础数据模型。

use serde::{Deserialize, Serialize};

/// 模型类型（PPO / SAC / DQN / A2C / 规则策略）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelType {
    PPO,
    SAC,
    DQN,
    A2C,
    RuleBased,
}

/// 动作类型（买入 / 卖出 / 持有）
///
/// 原名 `ActionType` 拆名而来：它与 inference 的 `TradeAction`（五值交易
/// 动作）、rl 的 `ActionSpaceKind`（动作空间类别）语义各不相同，改名消除
/// 三处同名枚举的混淆。Python 绑定对外的类名仍为 "ActionType"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EnsembleAction {
    Buy,
    Sell,
    Hold,
}

/// 投资组合状态
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PortfolioState {
    pub cash: f64,
    pub positions: Vec<Position>,
    pub total_value: f64,
    pub unrealized_pnl: f64,
}

/// 持仓信息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub quantity: f64,
    pub avg_price: f64,
    pub current_price: f64,
    pub pnl: f64,
}

/// 观测（模型输入）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub market_features: Vec<f64>,
    pub technical_indicators: Vec<f64>,
    pub portfolio_state: PortfolioState,
    pub time_features: Vec<f64>,
}

/// 动作（模型输出）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub action_type: EnsembleAction,
    pub symbol: Option<String>,
    pub quantity: Option<f64>,
    pub confidence: f64,
}

/// 动作概率分布（用于软投票）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionProbabilities {
    pub buy: f64,
    pub sell: f64,
    pub hold: f64,
}

impl ActionProbabilities {
    /// 构造概率分布并自动归一化
    pub fn new(buy: f64, sell: f64, hold: f64) -> Self {
        let total = buy + sell + hold;
        if total <= 0.0 {
            // 全 0 或负数 → 均匀分布
            return Self {
                buy: 1.0 / 3.0,
                sell: 1.0 / 3.0,
                hold: 1.0 / 3.0,
            };
        }
        Self {
            buy: buy / total,
            sell: sell / total,
            hold: hold / total,
        }
    }

    /// 转为 [buy, sell, hold] 顺序的向量
    pub fn to_vec(&self) -> Vec<f64> {
        vec![self.buy, self.sell, self.hold]
    }
}

/// 集成策略类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EnsembleStrategy {
    HardVote,
    SoftVote,
    WeightedVote,
    Stacking,
    DynamicWeighted,
}

/// 动作快照（不可变快照，用于报告/解释）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionSnapshot {
    pub action_type: EnsembleAction,
    pub symbol: Option<String>,
    pub quantity: Option<f64>,
    pub confidence: f64,
}

impl From<&Action> for ActionSnapshot {
    fn from(a: &Action) -> Self {
        Self {
            action_type: a.action_type,
            symbol: a.symbol.clone(),
            quantity: a.quantity,
            confidence: a.confidence,
        }
    }
}

/// 单个模型预测结果
#[derive(Debug, Clone, PartialEq)]
pub struct ModelPrediction {
    pub model_name: String,
    pub model_type: ModelType,
    pub action: Action,
    pub confidence: f64,
    pub action_probs: ActionProbabilities,
}

/// 模型权重
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelWeight {
    pub model_name: String,
    pub weight: f64,
    pub last_updated: u64,
}

/// 模型表现记录
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPerformance {
    pub model_name: String,
    pub accuracy: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub total_return: f64,
    pub sample_count: usize,
    pub last_evaluated: u64,
}

/// 堆叠元模型输入
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackingFeatures {
    pub model_predictions: Vec<f64>,
    pub model_confidences: Vec<f64>,
    pub observation_features: Vec<f64>,
}
