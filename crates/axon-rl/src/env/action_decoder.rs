//! 动作解码：RL 动作 → 订单

use crate::action::converter::{
    ActionConverter, ContinuousActionConverter, DiscreteActionConverter, Order, OrderType,
};
use crate::action::state::PortfolioState;
use crate::action::types::{Action, ActionSpace, ActionType, DiscreteAction as DiscreteActionEnum};
use crate::env::error::{EnvError, EnvResult};

/// 动作解码器：包装 `ActionConverter` 并处理环境特定的状态查询
pub struct ActionDecoder {
    /// 内部动作转换器
    converter: Box<dyn ActionConverter>,
}

impl ActionDecoder {
    /// 从离散动作空间构造解码器
    pub fn new_discrete(space: crate::action::types::DiscreteActionSpace, symbol: &str) -> Self {
        let converter = DiscreteActionConverter::new(space, symbol.to_string(), OrderType::Market);
        Self {
            converter: Box::new(converter),
        }
    }

    /// 从连续动作空间构造解码器
    pub fn new_continuous(
        space: crate::action::types::ContinuousActionSpace,
        symbol: &str,
    ) -> Self {
        let converter =
            ContinuousActionConverter::new(space, symbol.to_string(), OrderType::Market, 0.01);
        Self {
            converter: Box::new(converter),
        }
    }

    /// 从统一的 `ActionSpace` 构造解码器
    pub fn from_space(space: &ActionSpace, symbol: &str) -> EnvResult<Self> {
        match space {
            ActionSpace::Discrete(d) => Ok(Self::new_discrete(d.clone(), symbol)),
            ActionSpace::Continuous(c) => Ok(Self::new_continuous(c.clone(), symbol)),
        }
    }

    /// 动作 → 订单
    pub fn decode(&self, action: &Action, state: &PortfolioState) -> EnvResult<Option<Order>> {
        self.converter
            .to_order(action, state)
            .map_err(|e| EnvError::ActionError(e.to_string()))
    }

    /// 动作 → 目标权益占比 `∈ [-1, 1]`
    ///
    /// 这是**内核侧唯一的动作解码入口**（统一内核设计 6.3）：`SetTarget` 的
    /// `target_ratio` 直接由本方法给出，全链路不再有第二处动作 → 仓位换算。
    ///
    /// 与既有 [`Self::decode`] 的语义严格分离——后者产出具体订单数量（成交
    /// 口径），改动它会破坏现有模型的训练 / 评估，故另起新方法而不是改旧方法：
    ///
    /// - 连续空间：取第一个分量并 clip 到空间边界；
    /// - 离散空间：`Buy(bin)` → `+bin / n_bins`，`Sell(bin)` → `-bin / n_bins`，
    ///   `Hold` / 非法 index → `0.0`（离散语义没有「维持上次目标」的表达，
    ///   显式归零比猜测更安全）；
    /// - 两条分支的结果最后统一收敛到 `[-1, 1]`，保证 `SetTarget` 契约不被
    ///   宽于 `[-1, 1]` 的动作空间破坏。
    pub fn decode_target_ratio(&self, action: &Action) -> f64 {
        let ratio = match self.action_space() {
            ActionSpace::Continuous(space) => match &action.action_type {
                ActionType::Continuous(values) => values
                    .first()
                    .map(|value| space.clip(*value))
                    .unwrap_or(0.0),
                ActionType::Discrete(_) => 0.0,
            },
            ActionSpace::Discrete(space) => match action.action_type {
                ActionType::Discrete(index) => match space.index_to_action(index) {
                    Ok(DiscreteActionEnum::Hold) => 0.0,
                    Ok(DiscreteActionEnum::Buy(bin)) => bin.to_fraction(space.n_quantity_bins),
                    Ok(DiscreteActionEnum::Sell(bin)) => -bin.to_fraction(space.n_quantity_bins),
                    Err(_) => 0.0,
                },
                ActionType::Continuous(_) => 0.0,
            },
        };
        // 无论动作空间边界如何配置，`SetTarget` 契约只接受 [-1, 1]
        ratio.clamp(-1.0, 1.0)
    }

    /// 获取当前动作掩码
    pub fn mask(&self, state: &PortfolioState) -> Vec<bool> {
        self.converter.mask(state)
    }

    /// 获取动作空间引用
    pub fn action_space(&self) -> &ActionSpace {
        self.converter.action_space()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::converter::OrderSide;
    use crate::action::state::PortfolioState;
    use crate::action::types::{Action, DiscreteActionSpace, TradingDirection};

    fn empty_state() -> PortfolioState {
        PortfolioState {
            portfolio_value: 100_000.0,
            cash: 100_000.0,
            last_price: 5_000.0,
            ..Default::default()
        }
    }

    #[test]
    fn test_discrete_decoder_hold_returns_none() {
        let decoder = ActionDecoder::new_discrete(
            DiscreteActionSpace::new(3, TradingDirection::LongOnly),
            "BTCUSDT",
        );
        let action = Action::discrete(0); // Hold
        let order = decoder.decode(&action, &empty_state()).unwrap();
        assert!(order.is_none());
    }

    #[test]
    fn test_discrete_decoder_buy_returns_order() {
        let decoder = ActionDecoder::new_discrete(
            DiscreteActionSpace::new(3, TradingDirection::LongOnly),
            "BTCUSDT",
        );
        let action = Action::discrete(1); // Buy bin 1
        let order = decoder.decode(&action, &empty_state()).unwrap().unwrap();
        assert_eq!(order.side, OrderSide::Buy);
        assert_eq!(order.symbol, "BTCUSDT");
        assert!(order.quantity > 0.0);
    }

    #[test]
    fn test_continuous_decoder_returns_order() {
        let decoder = ActionDecoder::new_continuous(
            crate::action::types::ContinuousActionSpace::new(-1.0, 1.0),
            "BTCUSDT",
        );
        let action = Action::continuous(vec![0.5]);
        let order = decoder.decode(&action, &empty_state()).unwrap().unwrap();
        assert_eq!(order.side, OrderSide::Buy);
    }

    #[test]
    fn test_decode_target_ratio_continuous_clips_to_space() {
        let decoder = ActionDecoder::new_continuous(
            crate::action::types::ContinuousActionSpace::new(-1.0, 1.0),
            "BTCUSDT",
        );
        assert_eq!(
            decoder.decode_target_ratio(&Action::continuous(vec![0.5])),
            0.5
        );
        assert_eq!(
            decoder.decode_target_ratio(&Action::continuous(vec![-0.25])),
            -0.25
        );
        assert_eq!(
            decoder.decode_target_ratio(&Action::continuous(vec![3.0])),
            1.0
        );
        assert_eq!(
            decoder.decode_target_ratio(&Action::continuous(vec![-9.0])),
            -1.0
        );
        // 空向量 / 类型不匹配都退回 0.0（平仓），不 panic
        assert_eq!(
            decoder.decode_target_ratio(&Action::continuous(vec![])),
            0.0
        );
        assert_eq!(decoder.decode_target_ratio(&Action::discrete(1)), 0.0);

        // 动作空间边界宽于 SetTarget 契约时，硬性收敛到 [-1, 1]
        let wide = ActionDecoder::new_continuous(
            crate::action::types::ContinuousActionSpace::new(-2.0, 2.0),
            "BTCUSDT",
        );
        assert_eq!(
            wide.decode_target_ratio(&Action::continuous(vec![1.5])),
            1.0
        );
        assert_eq!(
            wide.decode_target_ratio(&Action::continuous(vec![-1.5])),
            -1.0
        );
    }

    #[test]
    fn test_decode_target_ratio_discrete_signs() {
        let decoder = ActionDecoder::new_discrete(
            DiscreteActionSpace::new(4, TradingDirection::Both),
            "BTCUSDT",
        );
        assert_eq!(
            decoder.decode_target_ratio(&Action::discrete(0)),
            0.0,
            "Hold"
        );
        assert_eq!(
            decoder.decode_target_ratio(&Action::discrete(2)),
            0.5,
            "Buy(bin 2) = 2 / 4"
        );
        assert_eq!(
            decoder.decode_target_ratio(&Action::discrete(5)),
            -0.25,
            "Sell(bin 1) = -1 / 4（index = n_bins + bin）"
        );
        assert_eq!(
            decoder.decode_target_ratio(&Action::discrete(999)),
            0.0,
            "非法 index 归零"
        );
    }
}
