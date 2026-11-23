//! 动作空间核心类型
//!
//! - `DiscreteAction` / `QuantityBin`：离散动作 + 量级分箱
//! - `DiscreteActionSpace`：离散动作空间配置（n 个动作 + 交易方向）
//! - `ContinuousActionSpace`：连续动作空间（目标仓位比例）
//! - `ActionSpace`：离散 / 连续的枚举
//! - `Action` / `ActionSpaceKind`：RL 智能体输出的统一动作表示
//! - `TradingDirection`：LongOnly / ShortOnly / Both

use serde::{Deserialize, Serialize};

// ── 交易方向 ──────────────────────────────────────────────

/// 交易方向约束
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradingDirection {
    /// 仅做多
    LongOnly,
    /// 仅做空
    ShortOnly,
    /// 双向交易
    Both,
}

// ── QuantityBin ───────────────────────────────────────────

/// 量级分箱：将连续仓位比例离散化为 bin
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuantityBin(pub usize);

impl QuantityBin {
    /// 将 bin 映射为仓位比例
    /// - `n_bins = 5` ⇒ `[0.2, 0.4, 0.6, 0.8, 1.0]`
    /// - `n_bins = 0` 或 `bin = 0` ⇒ 0
    pub fn to_fraction(self, n_bins: usize) -> f64 {
        if self.0 == 0 || n_bins == 0 {
            0.0
        } else {
            self.0 as f64 / n_bins as f64
        }
    }
}

// ── 离散动作 ──────────────────────────────────────────────

/// 离散动作枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiscreteAction {
    /// 持有当前仓位不变
    Hold,
    /// 买入指定量级
    Buy(QuantityBin),
    /// 卖出指定量级
    Sell(QuantityBin),
}

// ── 离散动作空间 ──────────────────────────────────────────

/// 离散动作空间
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscreteActionSpace {
    /// 量级分箱数（不含 Hold）
    pub n_quantity_bins: usize,
    /// 总动作数 = `1 + n_quantity_bins * 2`
    pub n: usize,
    /// 交易方向
    pub direction: TradingDirection,
}

impl DiscreteActionSpace {
    /// 构造离散动作空间
    pub fn new(n_quantity_bins: usize, direction: TradingDirection) -> Self {
        let n = 1 + n_quantity_bins * 2;
        Self {
            n_quantity_bins,
            n,
            direction,
        }
    }

    /// index → `DiscreteAction`
    /// - `0` ⇒ Hold
    /// - `1..=n_bins` ⇒ Buy(bin)
    /// - `n_bins+1..` ⇒ Sell(bin)
    pub fn index_to_action(
        &self,
        index: usize,
    ) -> Result<DiscreteAction, super::error::ActionError> {
        if index >= self.n {
            return Err(super::error::ActionError::InvalidIndex {
                index,
                size: self.n,
            });
        }
        if index == 0 {
            return Ok(DiscreteAction::Hold);
        }
        if index <= self.n_quantity_bins {
            Ok(DiscreteAction::Buy(QuantityBin(index)))
        } else {
            let bin = index - self.n_quantity_bins;
            Ok(DiscreteAction::Sell(QuantityBin(bin)))
        }
    }

    /// 返回动作掩码（`true` = 合法）
    pub fn valid_mask(&self, state: &super::state::PortfolioState) -> Vec<bool> {
        let mut mask = vec![true; self.n]; // Hold 总是合法

        match self.direction {
            TradingDirection::LongOnly => {
                // 不能做空：卖出仅在有持仓时合法
                for slot in mask.iter_mut().skip(self.n_quantity_bins + 1) {
                    *slot = state.position > 0.0;
                }
            }
            TradingDirection::ShortOnly => {
                // 仅做空：买入仅在有空头持仓（用于平仓）时合法
                for slot in mask.iter_mut().take(self.n_quantity_bins + 1).skip(1) {
                    *slot = state.position < 0.0;
                }
            }
            TradingDirection::Both => {
                // 做多：买入需要足够现金
                for (i, slot) in mask
                    .iter_mut()
                    .enumerate()
                    .take(self.n_quantity_bins + 1)
                    .skip(1)
                {
                    let fraction = QuantityBin(i).to_fraction(self.n_quantity_bins);
                    let required = fraction * state.portfolio_value;
                    *slot = state.cash >= required;
                }
                // 做空/平仓：卖出需要有持仓
                for slot in mask.iter_mut().skip(self.n_quantity_bins + 1) {
                    *slot = state.position != 0.0;
                }
            }
        }

        mask
    }
}

// ── 连续动作空间 ──────────────────────────────────────────

/// 连续动作空间：目标仓位比例 ∈ `[min, max]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuousActionSpace {
    /// 下界（默认 -1.0 = 满仓做空）
    pub min: f64,
    /// 上界（默认 +1.0 = 满仓做多）
    pub max: f64,
    /// 形状
    pub shape: Vec<usize>,
}

impl ContinuousActionSpace {
    /// 构造连续动作空间（默认单变量）
    pub fn new(min: f64, max: f64) -> Self {
        Self {
            min,
            max,
            shape: vec![1],
        }
    }

    /// clip 到合法范围
    pub fn clip(&self, value: f64) -> f64 {
        value.clamp(self.min, self.max)
    }
}

// ── ActionSpace 枚举 ─────────────────────────────────────

/// 动作空间（离散 / 连续）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionSpace {
    /// 离散动作空间
    Discrete(DiscreteActionSpace),
    /// 连续动作空间
    Continuous(ContinuousActionSpace),
}

impl ActionSpace {
    /// 离散空间大小（连续空间返回 `None`）
    pub fn gymnasium_n(&self) -> Option<usize> {
        match self {
            ActionSpace::Discrete(d) => Some(d.n),
            ActionSpace::Continuous(_) => None,
        }
    }

    /// Gymnasium 形状
    pub fn gymnasium_shape(&self) -> Vec<usize> {
        match self {
            ActionSpace::Discrete(_) => vec![1],
            ActionSpace::Continuous(c) => c.shape.clone(),
        }
    }

    /// Gymnasium 下界
    pub fn gymnasium_low(&self) -> Vec<f64> {
        match self {
            ActionSpace::Discrete(_) => vec![0.0],
            ActionSpace::Continuous(c) => {
                let n: usize = c.shape.iter().product();
                vec![c.min; n]
            }
        }
    }

    /// Gymnasium 上界
    pub fn gymnasium_high(&self) -> Vec<f64> {
        match self {
            ActionSpace::Discrete(d) => vec![(d.n - 1) as f64],
            ActionSpace::Continuous(c) => {
                let n: usize = c.shape.iter().product();
                vec![c.max; n]
            }
        }
    }
}

// ── Action / ActionSpaceKind ──────────────────────────────────

/// 动作类型标记
///
/// 原名 `ActionType` 拆名而来：它标记的是动作空间的类别（离散/连续），
/// 与 inference 的"交易动作"`TradeAction`、ensemble 的"集成动作"
/// `EnsembleAction` 是三种不同语义，改名消除三处同名枚举的混淆。
#[derive(Debug, Clone, PartialEq)]
pub enum ActionSpaceKind {
    /// 离散动作（选中的 index）
    Discrete(usize),
    /// 连续动作（值向量）
    Continuous(Vec<f64>),
}

/// 统一动作表示（网络输出 → 环境执行）
#[derive(Debug, Clone)]
pub struct Action {
    /// 原始动作值（来自策略网络，未经处理）
    pub raw: Vec<f64>,
    /// 处理后的合法值（clip / mask 后）
    pub processed: Vec<f64>,
    /// 动作类型
    pub action_type: ActionSpaceKind,
}

impl Action {
    /// 构造离散动作
    pub fn discrete(index: usize) -> Self {
        Self {
            raw: vec![index as f64],
            processed: vec![index as f64],
            action_type: ActionSpaceKind::Discrete(index),
        }
    }

    /// 构造连续动作
    pub fn continuous(values: Vec<f64>) -> Self {
        Self {
            raw: values.clone(),
            processed: values.clone(),
            action_type: ActionSpaceKind::Continuous(values),
        }
    }
}

/// 应用动作掩码到 logits（masked 位置设为极小值）
pub fn apply_action_mask(logits: &[f64], mask: &[bool]) -> Vec<f64> {
    debug_assert_eq!(logits.len(), mask.len());
    const LARGE_NEG: f64 = -1e9;
    logits
        .iter()
        .zip(mask.iter())
        .map(|(&logit, &valid)| if valid { logit } else { LARGE_NEG })
        .collect()
}
