//! 动作空间模块测试
//!
//! 覆盖：类型构造、DiscreteActionSpace / ContinuousActionSpace 行为、
//! 转换器、掩码、动作平滑器、`apply_action_mask` 工具函数、错误处理。

use super::*;
use crate::action::converter::ActionConverter;
use crate::action::converter::{
    ContinuousActionConverter, DiscreteActionConverter, OrderSide, OrderType,
};
use crate::action::error::{ActionError, ActionResult};
use crate::action::state::PortfolioState;
use crate::action::types::{
    Action, ActionSpace, ActionSpaceKind, ContinuousActionSpace, DiscreteAction,
    DiscreteActionSpace, QuantityBin, TradingDirection, apply_action_mask,
};

// ── PortfolioState 测试 ──────────────────────────────────

fn sample_state() -> PortfolioState {
    PortfolioState {
        position: 10.0,
        cash: 50_000.0,
        portfolio_value: 100_000.0,
        margin_used: 0.0,
        margin_available: 100_000.0,
        unrealized_pnl: 0.0,
        last_price: 5_000.0,
    }
}

#[test]
fn test_portfolio_state_position_value() {
    let s = sample_state();
    // 10 × 5000 = 50,000
    assert_eq!(s.position_value(), 50_000.0);
}

#[test]
fn test_portfolio_state_position_ratio() {
    let s = sample_state();
    // 50,000 / 100,000 = 0.5
    assert!((s.position_ratio() - 0.5).abs() < 1e-10);
}

#[test]
fn test_portfolio_state_is_flat() {
    let mut s = sample_state();
    assert!(!s.is_flat());
    s.position = 0.0;
    assert!(s.is_flat());
}

#[test]
fn test_portfolio_state_zero_value() {
    let mut s = sample_state();
    s.portfolio_value = 0.0;
    assert_eq!(s.position_ratio(), 0.0);
}

// ── QuantityBin 测试 ────────────────────────────────────

#[test]
fn test_quantity_bin_to_fraction() {
    assert_eq!(QuantityBin(0).to_fraction(5), 0.0);
    assert_eq!(QuantityBin(1).to_fraction(5), 0.2);
    assert_eq!(QuantityBin(3).to_fraction(5), 0.6);
    assert_eq!(QuantityBin(5).to_fraction(5), 1.0);
    // 0 bins ⇒ 全 0
    assert_eq!(QuantityBin(5).to_fraction(0), 0.0);
}

// ── DiscreteActionSpace 测试 ────────────────────────────

#[test]
fn test_discrete_action_space_new() {
    let s = DiscreteActionSpace::new(3, TradingDirection::LongOnly);
    assert_eq!(s.n_quantity_bins, 3);
    assert_eq!(s.n, 7); // 1 + 3 + 3
}

#[test]
fn test_discrete_action_space_index_to_action() {
    let s = DiscreteActionSpace::new(3, TradingDirection::LongOnly);
    assert_eq!(s.index_to_action(0).unwrap(), DiscreteAction::Hold);
    assert_eq!(
        s.index_to_action(1).unwrap(),
        DiscreteAction::Buy(QuantityBin(1))
    );
    assert_eq!(
        s.index_to_action(3).unwrap(),
        DiscreteAction::Buy(QuantityBin(3))
    );
    assert_eq!(
        s.index_to_action(4).unwrap(),
        DiscreteAction::Sell(QuantityBin(1))
    );
    assert_eq!(
        s.index_to_action(6).unwrap(),
        DiscreteAction::Sell(QuantityBin(3))
    );
}

#[test]
fn test_discrete_action_space_out_of_range() {
    let s = DiscreteActionSpace::new(3, TradingDirection::LongOnly);
    assert!(matches!(
        s.index_to_action(7),
        Err(ActionError::InvalidIndex { .. })
    ));
    assert!(matches!(
        s.index_to_action(100),
        Err(ActionError::InvalidIndex { .. })
    ));
}

#[test]
fn test_discrete_mask_long_only_no_position() {
    let s = DiscreteActionSpace::new(3, TradingDirection::LongOnly);
    let state = PortfolioState {
        position: 0.0,
        ..sample_state()
    };
    let mask = s.valid_mask(&state);
    // Hold, Buy1, Buy2, Buy3 ⇒ true
    assert!(mask[0]);
    assert!(mask[1]);
    assert!(mask[2]);
    assert!(mask[3]);
    // Sell1, Sell2, Sell3 ⇒ false (no position)
    assert!(!mask[4]);
    assert!(!mask[5]);
    assert!(!mask[6]);
}

#[test]
fn test_discrete_mask_long_only_with_position() {
    let s = DiscreteActionSpace::new(3, TradingDirection::LongOnly);
    let state = sample_state(); // position > 0
    let mask = s.valid_mask(&state);
    // 全部合法
    assert!(mask.iter().all(|&x| x));
}

#[test]
fn test_discrete_mask_short_only() {
    let s = DiscreteActionSpace::new(3, TradingDirection::ShortOnly);
    // 空仓：Buy 不合法
    let state = PortfolioState {
        position: 0.0,
        ..sample_state()
    };
    let mask = s.valid_mask(&state);
    assert!(mask[0]); // Hold
    assert!(!mask[1]); // Buy 1 invalid
    // Sell 合法
    assert!(mask[4]);
}

#[test]
fn test_discrete_mask_both_insufficient_cash() {
    let s = DiscreteActionSpace::new(3, TradingDirection::Both);
    let state = PortfolioState {
        position: 0.0,
        cash: 10.0,
        portfolio_value: 1_000_000.0,
        ..sample_state()
    };
    let mask = s.valid_mask(&state);
    // Buy 全部不合法（现金不足）
    assert!(!mask[1]);
    assert!(!mask[2]);
    assert!(!mask[3]);
    // Sell 全部不合法（无持仓）
    assert!(!mask[4]);
    assert!(!mask[5]);
    assert!(!mask[6]);
}

// ── ContinuousActionSpace 测试 ──────────────────────────

#[test]
fn test_continuous_action_space_clip() {
    let s = ContinuousActionSpace::new(-1.0, 1.0);
    assert_eq!(s.clip(0.5), 0.5);
    assert_eq!(s.clip(1.5), 1.0);
    assert_eq!(s.clip(-2.0), -1.0);
    assert_eq!(s.clip(0.0), 0.0);
}

#[test]
fn test_continuous_action_space_custom_range() {
    let s = ContinuousActionSpace::new(0.0, 0.5);
    assert_eq!(s.clip(0.3), 0.3);
    assert_eq!(s.clip(0.7), 0.5);
    assert_eq!(s.clip(-0.1), 0.0);
}

// ── ActionSpace 枚举测试 ───────────────────────────────

#[test]
fn test_action_space_gymnasium_discrete() {
    let s = ActionSpace::Discrete(DiscreteActionSpace::new(3, TradingDirection::LongOnly));
    assert_eq!(s.gymnasium_n(), Some(7));
    assert_eq!(s.gymnasium_shape(), vec![1]);
    assert_eq!(s.gymnasium_low(), vec![0.0]);
    assert_eq!(s.gymnasium_high(), vec![6.0]);
}

#[test]
fn test_action_space_gymnasium_continuous() {
    let s = ActionSpace::Continuous(ContinuousActionSpace::new(-1.0, 1.0));
    assert_eq!(s.gymnasium_n(), None);
    assert_eq!(s.gymnasium_shape(), vec![1]);
    assert_eq!(s.gymnasium_low(), vec![-1.0]);
    assert_eq!(s.gymnasium_high(), vec![1.0]);
}

// ── Action / ActionSpaceKind 测试 ───────────────────────────

#[test]
fn test_action_discrete() {
    let a = Action::discrete(3);
    assert_eq!(a.action_type, ActionSpaceKind::Discrete(3));
    assert_eq!(a.raw, vec![3.0]);
}

#[test]
fn test_action_continuous() {
    let a = Action::continuous(vec![0.5, -0.3]);
    assert_eq!(a.action_type, ActionSpaceKind::Continuous(vec![0.5, -0.3]));
    assert_eq!(a.raw, vec![0.5, -0.3]);
}

// ── DiscreteActionConverter 测试 ───────────────────────

#[test]
fn test_discrete_to_order_hold() {
    let c = DiscreteActionConverter::new(
        DiscreteActionSpace::new(3, TradingDirection::LongOnly),
        "BTCUSDT",
        OrderType::Market,
    );
    let action = Action::discrete(0);
    let order = c.to_order(&action, &sample_state()).unwrap();
    assert!(order.is_none());
}

#[test]
fn test_discrete_to_order_buy_100pct() {
    let c = DiscreteActionConverter::new(
        DiscreteActionSpace::new(3, TradingDirection::LongOnly),
        "BTCUSDT",
        OrderType::Market,
    );
    // index 3 = Buy 100% of portfolio
    let action = Action::discrete(3);
    let order = c.to_order(&action, &sample_state()).unwrap().unwrap();
    assert_eq!(order.side, OrderSide::Buy);
    // 100,000 / 5,000 = 20
    assert!((order.quantity - 20.0).abs() < 1e-6);
}

#[test]
fn test_discrete_to_order_sell_50pct() {
    let c = DiscreteActionConverter::new(
        DiscreteActionSpace::new(4, TradingDirection::LongOnly),
        "BTCUSDT",
        OrderType::Market,
    );
    // n=4, index 5 = Sell bin 1 = 1/4 = 25% of position
    let action = Action::discrete(5);
    let state = sample_state(); // position=10
    let order = c.to_order(&action, &state).unwrap().unwrap();
    assert_eq!(order.side, OrderSide::Sell);
    // 0.25 × 10 = 2.5
    assert!((order.quantity - 2.5).abs() < 1e-6);
}

#[test]
fn test_discrete_to_order_sell_no_position_errors() {
    let c = DiscreteActionConverter::new(
        DiscreteActionSpace::new(3, TradingDirection::LongOnly),
        "BTCUSDT",
        OrderType::Market,
    );
    let action = Action::discrete(4); // Sell 1
    let mut state = sample_state();
    state.position = 0.0;
    let result = c.to_order(&action, &state);
    assert!(matches!(result, Err(ActionError::NoPositionToSell { .. })));
}

#[test]
fn test_discrete_to_order_continuous_action_errors() {
    let c = DiscreteActionConverter::new(
        DiscreteActionSpace::new(3, TradingDirection::LongOnly),
        "BTCUSDT",
        OrderType::Market,
    );
    let action = Action::continuous(vec![0.5]);
    let result = c.to_order(&action, &sample_state());
    assert!(matches!(result, Err(ActionError::OutOfRange { .. })));
}

#[test]
fn test_discrete_to_order_zero_last_price() {
    let c = DiscreteActionConverter::new(
        DiscreteActionSpace::new(3, TradingDirection::LongOnly),
        "BTCUSDT",
        OrderType::Market,
    );
    let action = Action::discrete(3);
    let mut state = sample_state();
    state.last_price = 0.0;
    let order = c.to_order(&action, &state).unwrap();
    assert!(order.is_none());
}

// ── ContinuousActionConverter 测试 ─────────────────────

fn empty_state() -> PortfolioState {
    PortfolioState {
        position: 0.0,
        cash: 100_000.0,
        portfolio_value: 100_000.0,
        margin_used: 0.0,
        margin_available: 100_000.0,
        unrealized_pnl: 0.0,
        last_price: 5_000.0,
    }
}

#[test]
fn test_continuous_to_order_buy_50pct() {
    let c = ContinuousActionConverter::new(
        ContinuousActionSpace::new(-1.0, 1.0),
        "BTCUSDT",
        OrderType::Market,
        0.01,
    );
    let action = Action::continuous(vec![0.5]);
    let state = empty_state(); // position=0
    let order = c.to_order(&action, &state).unwrap().unwrap();
    assert_eq!(order.side, OrderSide::Buy);
    // target: 0.5 × 100,000 / 5,000 = 10
    assert!((order.quantity - 10.0).abs() < 1e-6);
}

#[test]
fn test_continuous_to_order_below_threshold() {
    let c = ContinuousActionConverter::new(
        ContinuousActionSpace::new(-1.0, 1.0),
        "BTCUSDT",
        OrderType::Market,
        0.05, // 5% 阈值
    );
    let action = Action::continuous(vec![0.02]); // 2% 变化 < 5%
    let order = c.to_order(&action, &empty_state()).unwrap();
    assert!(order.is_none());
}

#[test]
fn test_continuous_to_order_clip() {
    let c = ContinuousActionConverter::new(
        ContinuousActionSpace::new(-1.0, 1.0),
        "BTCUSDT",
        OrderType::Market,
        0.0,
    );
    let action = Action::continuous(vec![5.0]); // 超出 clip 上界
    let state = empty_state();
    let order = c.to_order(&action, &state).unwrap().unwrap();
    // clip 到 1.0 ⇒ target 100% 多头
    assert_eq!(order.side, OrderSide::Buy);
    // 1.0 × 100,000 / 5,000 - 0 = 20
    assert!((order.quantity - 20.0).abs() < 1e-6);
}

#[test]
fn test_continuous_to_order_short() {
    let c = ContinuousActionConverter::new(
        ContinuousActionSpace::new(-1.0, 1.0),
        "BTCUSDT",
        OrderType::Market,
        0.0,
    );
    let action = Action::continuous(vec![-0.5]);
    let state = empty_state();
    let order = c.to_order(&action, &state).unwrap().unwrap();
    assert_eq!(order.side, OrderSide::Sell);
    // target: -0.5 × 100,000 / 5,000 = -10, current = 0, |delta|=0.5 > 0
    // quantity = |target - current| = 10
    assert!((order.quantity - 10.0).abs() < 1e-6);
}

#[test]
fn test_continuous_to_order_zero_values() {
    let c = ContinuousActionConverter::new(
        ContinuousActionSpace::new(-1.0, 1.0),
        "BTCUSDT",
        OrderType::Market,
        0.0,
    );
    let action = Action::continuous(vec![0.5]);
    let mut state = empty_state();
    state.portfolio_value = 0.0;
    let order = c.to_order(&action, &state).unwrap();
    assert!(order.is_none());
}

#[test]
fn test_continuous_to_order_continuous_action_required() {
    let c = ContinuousActionConverter::new(
        ContinuousActionSpace::new(-1.0, 1.0),
        "BTCUSDT",
        OrderType::Market,
        0.0,
    );
    let action = Action::discrete(2); // 离散动作不合法
    let result = c.to_order(&action, &empty_state());
    assert!(matches!(result, Err(ActionError::OutOfRange { .. })));
}

// ── ActionSmoother 测试 ────────────────────────────────

#[test]
fn test_smoother_limits_delta() {
    let mut s = ActionSmoother::new(0.1, 0.5);
    // prev=0, target=1.0 ⇒ smoothed=0.5, delta=0.5 > 0.1 ⇒ clamp to 0.1
    let s1 = s.smooth(1.0);
    assert!((s1 - 0.1).abs() < 1e-10);
    // prev=0.1, target=1.0 ⇒ smoothed=0.55, delta=0.45 > 0.1 ⇒ clamp to 0.2
    let s2 = s.smooth(1.0);
    assert!((s2 - 0.2).abs() < 1e-10);
}

#[test]
fn test_smoother_no_clamp_needed() {
    let mut s = ActionSmoother::new(0.5, 0.5);
    // prev=0, target=0.3 ⇒ smoothed=0.15, delta=0.15 < 0.5 ⇒ no clamp
    let s1 = s.smooth(0.3);
    assert!((s1 - 0.15).abs() < 1e-10);
}

#[test]
fn test_smoother_reset() {
    let mut s = ActionSmoother::new(0.1, 0.5);
    s.smooth(1.0);
    s.reset();
    assert_eq!(s.prev_smoothed, 0.0);
}

#[test]
fn test_smoother_negative_delta() {
    let mut s = ActionSmoother::new(0.2, 0.5);
    s.smooth(0.0); // prev = 0
    // prev=0, target=-0.5
    // smoothed = 0.5 * -0.5 + 0.5 * 0 = -0.25
    // delta = -0.25 - 0 = -0.25
    // |delta| = 0.25 > 0.2 ⇒ clamp
    // clamped = 0 + (-1) * 0.2 = -0.2
    let s2 = s.smooth(-0.5);
    assert!((s2 - (-0.2)).abs() < 1e-10, "expected -0.2, got {}", s2);
}

// ── apply_action_mask 测试 ─────────────────────────────

#[test]
fn test_apply_action_mask_basic() {
    let logits = vec![1.0, 2.0, 3.0, 4.0];
    let mask = vec![true, false, true, false];
    let masked = apply_action_mask(&logits, &mask);
    assert_eq!(masked[0], 1.0);
    assert_eq!(masked[1], -1e9);
    assert_eq!(masked[2], 3.0);
    assert_eq!(masked[3], -1e9);
}

#[test]
fn test_apply_action_mask_all_true() {
    let logits = vec![1.0, 2.0];
    let mask = vec![true, true];
    let masked = apply_action_mask(&logits, &mask);
    assert_eq!(masked, vec![1.0, 2.0]);
}

// ── validate_action 测试 ───────────────────────────────

#[test]
fn test_validate_action_discrete_ok() {
    let space = ActionSpace::Discrete(DiscreteActionSpace::new(3, TradingDirection::LongOnly));
    let action = Action::discrete(0);
    let result: ActionResult<()> =
        crate::action::error::validate_action(&action, &space, &sample_state());
    assert!(result.is_ok());
}

#[test]
fn test_validate_action_discrete_masked() {
    let space = ActionSpace::Discrete(DiscreteActionSpace::new(3, TradingDirection::LongOnly));
    let action = Action::discrete(4); // Sell
    let mut state = sample_state();
    state.position = 0.0;
    let result = crate::action::error::validate_action(&action, &space, &state);
    assert!(matches!(result, Err(ActionError::Masked { .. })));
}

#[test]
fn test_validate_action_continuous_ok() {
    let space = ActionSpace::Continuous(ContinuousActionSpace::new(-1.0, 1.0));
    let action = Action::continuous(vec![0.5]);
    let result = crate::action::error::validate_action(&action, &space, &sample_state());
    assert!(result.is_ok());
}

#[test]
fn test_validate_action_continuous_out_of_range() {
    let space = ActionSpace::Continuous(ContinuousActionSpace::new(-1.0, 1.0));
    let action = Action::continuous(vec![5.0]);
    let result = crate::action::error::validate_action(&action, &space, &sample_state());
    assert!(matches!(result, Err(ActionError::OutOfRange { .. })));
}
