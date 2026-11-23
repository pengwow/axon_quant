//! 动作转换器：把 RL 动作映射到具体订单
//!
//! - `ActionConverter` trait
//! - `DiscreteActionConverter`：Hold/Buy/Sell + 量级分箱
//! - `ContinuousActionConverter`：目标仓位比例

use crate::action::error::{ActionError, ActionResult};
use crate::action::state::PortfolioState;
use crate::action::types::{
    Action, ActionSpace, ActionSpaceKind, ContinuousActionSpace,
    DiscreteAction as DiscreteActionEnum, DiscreteActionSpace, QuantityBin,
};

// ── 简化订单类型 ─────────────────────────────────────────
// 仍不依赖 `axon_core::order` 的 Order/OrderType 结构（保持 RL 环境的独立
// 可测性），但订单方向复用 canonical 枚举，避免第四份 Side 重复定义。

/// 订单方向（re-export [`axon_core::market::Side`]）
pub use axon_core::market::Side as OrderSide;

/// 订单类型（简化版）
#[derive(Debug, Clone, PartialEq)]
pub enum OrderType {
    /// 市价单
    Market,
    /// 限价单
    Limit {
        /// 限价
        price: f64,
    },
}

/// 简化订单（不依赖 `axon-core::order`，便于在测试中独立使用）
#[derive(Debug, Clone, PartialEq)]
pub struct Order {
    /// 标的代码
    pub symbol: String,
    /// 买卖方向
    pub side: OrderSide,
    /// 数量
    pub quantity: f64,
    /// 订单类型
    pub order_type: OrderType,
}

// ── ActionConverter trait ────────────────────────────────

/// 动作转换器 trait
pub trait ActionConverter: Send + Sync {
    /// 把动作转换为订单（`None` 表示无需下单，如 Hold 或 delta 过小）
    fn to_order(&self, action: &Action, state: &PortfolioState) -> ActionResult<Option<Order>>;
    /// 动作掩码
    fn mask(&self, state: &PortfolioState) -> Vec<bool>;
    /// 动作空间
    fn action_space(&self) -> &ActionSpace;
}

// ── 离散动作转换器 ───────────────────────────────────────

/// 离散动作转换器
pub struct DiscreteActionConverter {
    /// 动作空间
    pub space: DiscreteActionSpace,
    /// 缓存的 `ActionSpace`（避免 `action_space()` 中的生命周期问题）
    action_space_cache: ActionSpace,
    /// 标的代码
    pub symbol: String,
    /// 默认订单类型
    pub order_type: OrderType,
}

impl DiscreteActionConverter {
    /// 构造离散动作转换器
    pub fn new(
        space: DiscreteActionSpace,
        symbol: impl Into<String>,
        order_type: OrderType,
    ) -> Self {
        let action_space_cache = ActionSpace::Discrete(space.clone());
        Self {
            space,
            action_space_cache,
            symbol: symbol.into(),
            order_type,
        }
    }
}

impl ActionConverter for DiscreteActionConverter {
    fn to_order(&self, action: &Action, state: &PortfolioState) -> ActionResult<Option<Order>> {
        let index = match action.action_type {
            ActionSpaceKind::Discrete(i) => i,
            ActionSpaceKind::Continuous(_) => {
                return Err(ActionError::OutOfRange {
                    value: action.raw.first().copied().unwrap_or(0.0),
                    min: 0.0,
                    max: self.space.n as f64,
                });
            }
        };

        let discrete = self.space.index_to_action(index)?;

        match discrete {
            DiscreteActionEnum::Hold => Ok(None),
            DiscreteActionEnum::Buy(bin) => {
                let fraction = bin.to_fraction(self.space.n_quantity_bins);
                let target_value = fraction * state.portfolio_value;
                if state.last_price <= 0.0 {
                    return Ok(None);
                }
                let quantity = target_value / state.last_price;
                Ok(Some(Order {
                    symbol: self.symbol.clone(),
                    side: OrderSide::Buy,
                    quantity,
                    order_type: self.order_type.clone(),
                }))
            }
            DiscreteActionEnum::Sell(bin) => {
                let fraction = bin.to_fraction(self.space.n_quantity_bins);
                let quantity = fraction * state.position.abs();
                if quantity <= 0.0 {
                    return Err(ActionError::NoPositionToSell {
                        position: state.position,
                    });
                }
                Ok(Some(Order {
                    symbol: self.symbol.clone(),
                    side: OrderSide::Sell,
                    quantity,
                    order_type: self.order_type.clone(),
                }))
            }
        }
    }

    fn mask(&self, state: &PortfolioState) -> Vec<bool> {
        self.space.valid_mask(state)
    }

    fn action_space(&self) -> &ActionSpace {
        &self.action_space_cache
    }
}

// ── 连续动作转换器 ───────────────────────────────────────

/// 连续动作转换器：把目标仓位比例转换为订单
pub struct ContinuousActionConverter {
    /// 动作空间
    pub space: ContinuousActionSpace,
    /// 缓存的 `ActionSpace`
    action_space_cache: ActionSpace,
    /// 标的代码
    pub symbol: String,
    /// 默认订单类型
    pub order_type: OrderType,
    /// 最小交易阈值（仓位变化小于此值不执行）
    pub min_trade_threshold: f64,
}

impl ContinuousActionConverter {
    /// 构造连续动作转换器
    pub fn new(
        space: ContinuousActionSpace,
        symbol: impl Into<String>,
        order_type: OrderType,
        min_trade_threshold: f64,
    ) -> Self {
        let action_space_cache = ActionSpace::Continuous(space.clone());
        Self {
            space,
            action_space_cache,
            symbol: symbol.into(),
            order_type,
            min_trade_threshold,
        }
    }
}

impl ActionConverter for ContinuousActionConverter {
    fn to_order(&self, action: &Action, state: &PortfolioState) -> ActionResult<Option<Order>> {
        let target_ratio = match action.action_type {
            ActionSpaceKind::Continuous(ref vals) if !vals.is_empty() => self.space.clip(vals[0]),
            _ => {
                return Err(ActionError::OutOfRange {
                    value: action.raw.first().copied().unwrap_or(0.0),
                    min: self.space.min,
                    max: self.space.max,
                });
            }
        };

        if state.portfolio_value <= 0.0 || state.last_price <= 0.0 {
            return Ok(None);
        }

        let current_ratio = state.position_ratio();
        let delta = target_ratio - current_ratio;
        if delta.abs() < self.min_trade_threshold {
            return Ok(None);
        }

        let target_position = target_ratio * state.portfolio_value / state.last_price;
        let quantity = (target_position - state.position).abs();
        if quantity <= 0.0 {
            return Ok(None);
        }
        let side = if delta > 0.0 {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };
        Ok(Some(Order {
            symbol: self.symbol.clone(),
            side,
            quantity,
            order_type: self.order_type.clone(),
        }))
    }

    fn mask(&self, _state: &PortfolioState) -> Vec<bool> {
        // 连续空间的 mask 通常不适用（所有值在 clip 后都合法）
        vec![true]
    }

    fn action_space(&self) -> &ActionSpace {
        &self.action_space_cache
    }
}

// 抑制未使用导入告警
#[allow(dead_code)]
fn _ensure_quantity_bin_in_scope() {
    let _ = QuantityBin(0);
}
