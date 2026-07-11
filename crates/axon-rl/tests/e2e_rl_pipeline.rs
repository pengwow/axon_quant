//! 端到端测试:axon-rl 强化学习环境完整流程
//!
//! ## 5 个测试场景
//!
//! 1. `rl_trading_env_reset_and_step`:创建环境 → reset → 多步 step → 验证 obs/reward/done
//! 2. `rl_action_space_index_roundtrip`:离散动作空间 index → action → index roundtrip
//! 3. `rl_pnl_reward_calculation`:PnL 奖励函数 → 盈利/亏损验证
//! 4. `rl_env_config_serialization`:EnvConfig JSON 序列化 → 反序列化 → 字段一致
//! 5. `rl_episode_completes_at_max_steps`:运行到 max_steps → done=true
//!
//! 运行:`cargo test -p axon-rl --test e2e_rl_pipeline`

use axon_rl::action::state::PortfolioState;
use axon_rl::{
    Action, ActionSpace, DiscreteActionSpace, EnvConfig, MarketBar, PnLReward, RewardFn,
    TradingDirection,
};

// ── helpers ────────────────────────────────────────────────────────────

/// 构造递增价格的 K 线数据
fn rising_market(n: usize) -> Vec<MarketBar> {
    (0..n)
        .map(|i| {
            let price = 100.0 + i as f64;
            MarketBar::new(
                i as u64 * 60_000,
                price - 0.5,
                price + 1.0,
                price - 1.0,
                price,
                1000.0,
            )
        })
        .collect()
}

/// 构造测试用环境配置
fn test_config() -> EnvConfig {
    EnvConfig {
        initial_capital: 100_000.0,
        transaction_cost: 0.001,
        slippage: 0.0005,
        max_position_ratio: 1.0,
        max_steps: 100,
        seed: Some(42),
        symbol: "BTCUSDT".to_string(),
        return_window: 252,
    }
}

/// 构造测试用环境
fn make_env(n_steps: usize) -> axon_rl::TradingEnv {
    let config = EnvConfig {
        max_steps: n_steps,
        ..test_config()
    };
    let action_space =
        ActionSpace::Discrete(DiscreteActionSpace::new(5, TradingDirection::LongOnly));
    let market_data = rising_market(n_steps + 1);
    let reward_fn = Box::new(PnLReward::default());
    let observation_space = Box::new(
        axon_rl::DefaultObservationSpace::new(
            5,
            vec![axon_rl::FeatureConfig {
                name: "close".into(),
                source: axon_rl::FeatureSource::PriceField("close".into()),
                normalizer: axon_rl::NormalizerType::ZScore,
                clip_range: Some((-5.0, 5.0)),
            }],
        )
        .unwrap(),
    );

    axon_rl::TradingEnv::new(
        config,
        action_space,
        observation_space,
        reward_fn,
        market_data,
    )
    .unwrap()
}

fn make_action(index: usize) -> Action {
    Action::discrete(index)
}

// ── 1. reset + 多步 step → 验 obs/reward/done ────────────────────────

#[test]
fn rl_trading_env_reset_and_step() {
    let mut env = make_env(10);

    let obs = env.reset().unwrap();
    assert!(!obs.features.is_empty(), "观测特征不应为空");

    let mut done = false;
    for step in 0..5 {
        assert!(!done, "第 {step} 步不应结束");
        let action = make_action(0); // Hold
        let (_obs, _reward, done_flag, info) = env.step(&action).unwrap();
        assert!(info.current_step > 0);
        done = done_flag;
    }
}

// ── 2. 离散动作空间: index → action → index roundtrip ─────────────────

#[test]
fn rl_action_space_index_roundtrip() {
    let space = DiscreteActionSpace::new(5, TradingDirection::LongOnly);

    // 0 → Hold → 0
    let action = space.index_to_action(0).unwrap();
    let idx = action_to_index(&space, &action);
    assert_eq!(idx, 0);

    // 1 → Buy(1) → 1
    let action = space.index_to_action(1).unwrap();
    let idx = action_to_index(&space, &action);
    assert_eq!(idx, 1);

    // 6 → Sell(1) → 6
    let action = space.index_to_action(6).unwrap();
    let idx = action_to_index(&space, &action);
    assert_eq!(idx, 6);

    // 10 → Sell(5) → 10
    let action = space.index_to_action(10).unwrap();
    let idx = action_to_index(&space, &action);
    assert_eq!(idx, 10);
}

fn action_to_index(space: &DiscreteActionSpace, action: &axon_rl::DiscreteAction) -> usize {
    use axon_rl::DiscreteAction;
    match action {
        DiscreteAction::Hold => 0,
        DiscreteAction::Buy(bin) => bin.0,
        DiscreteAction::Sell(bin) => space.n_quantity_bins + bin.0,
    }
}

// ── 3. PnL 奖励函数: 盈利/亏损验证 ────────────────────────────────────

#[test]
fn rl_pnl_reward_calculation() {
    let reward_fn = PnLReward::default();

    // 盈利场景
    let state = PortfolioState {
        cash: 0.0,
        portfolio_value: 100_000.0,
        position: 1.0,
        last_price: 100.0,
        ..Default::default()
    };
    let next_state = PortfolioState {
        cash: 0.0,
        portfolio_value: 101_000.0,
        position: 1.0,
        last_price: 101.0,
        ..Default::default()
    };
    let action = Action::discrete(0);
    let reward = reward_fn
        .calculate(&state, &action, &next_state, &[])
        .unwrap();
    assert!(reward > 0.0, "盈利应产生正奖励,实为 {reward}");

    // 亏损场景
    let next_state_loss = PortfolioState {
        cash: 0.0,
        portfolio_value: 99_000.0,
        position: 1.0,
        last_price: 99.0,
        ..Default::default()
    };
    let reward_loss = reward_fn
        .calculate(&state, &action, &next_state_loss, &[])
        .unwrap();
    assert!(reward_loss < 0.0, "亏损应产生负奖励,实为 {reward_loss}");
}

// ── 4. EnvConfig JSON 序列化 roundtrip ────────────────────────────────

#[test]
fn rl_env_config_serialization() {
    let config = test_config();
    let json = serde_json::to_string(&config).unwrap();
    let restored: EnvConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.initial_capital, config.initial_capital);
    assert_eq!(restored.transaction_cost, config.transaction_cost);
    assert_eq!(restored.max_steps, config.max_steps);
    assert_eq!(restored.symbol, config.symbol);
}

// ── 5. 运行到 max_steps → done=true ──────────────────────────────────

#[test]
fn rl_episode_completes_at_max_steps() {
    let n = 20;
    let mut env = make_env(n);
    let _ = env.reset();

    let mut final_done = false;
    for _ in 0..n + 5 {
        let action = make_action(0);
        let (_, _, done, _) = env.step(&action).unwrap();
        final_done = done;
        if done {
            break;
        }
    }

    assert!(final_done, "运行到 max_steps 后 done 应为 true");
}
