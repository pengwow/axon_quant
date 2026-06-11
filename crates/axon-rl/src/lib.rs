//! AXON 强化学习环境
//!
//! 提供 Gymnasium 兼容的交易环境接口，包装 `axon-backtest` 回测引擎。
//!
//! # 模块规划
//!
//! | 模块 | 阶段 | 说明 |
//! |------|------|------|
//! | [`observation`] | Phase 1B P0 | 观测空间：特征工程 + 归一化 + 窗口 |
//! | [`action`] | Phase 1B P0 | 动作空间：Discrete / Box / MultiDiscrete |
//! | [`reward`] | Phase 1B P0 | 奖励函数：PnL / Sharpe / Sortino / 自定义 |
//! | `env` 模块 | Phase 1B P0 | 交易环境：整合观测 / 动作 / 奖励 / 回测 |
//! | [`vec_env`] | Phase 1B P1 | 向量化环境：并行 rollout |
//! | `python` | Phase 1B P0 | PyO3 绑定（feature = `python`） |

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod action;
pub mod env;
pub mod observation;
pub mod reward;
pub mod vec_env;

#[cfg(feature = "python")]
pub mod python;

pub use action::converter::{
    ActionConverter, ContinuousActionConverter, DiscreteActionConverter, Order, OrderSide,
    OrderType,
};
pub use action::error::{ActionError, ActionResult, validate_action};
pub use action::smoother::ActionSmoother;
pub use action::state::PortfolioState;
pub use action::types::{
    Action, ActionSpace, ActionType, ContinuousActionSpace, DiscreteAction, DiscreteActionSpace,
    QuantityBin, TradingDirection, apply_action_mask,
};
pub use env::action_decoder::ActionDecoder;
pub use env::config::EnvConfig;
pub use env::error::{EnvError, EnvResult};
pub use env::executor::Executor;
pub use env::trading_env::{StepResult, TradingEnv};
pub use env::types::{EnvInfo, ExecutionResult, MarketBar};
pub use observation::buffer::TickBuffer;
pub use observation::normalizer::{
    MinMaxNormalizer, NoopNormalizer, Normalizer, RobustNormalizer, RunningStats, ZScoreNormalizer,
    make_normalizer,
};
pub use observation::space::DefaultObservationSpace;
pub use observation::types::{
    AggregationType, BoxSpace, DType, FeatureConfig, FeatureSource, MarketState, NormalizerType,
    Observation, ObservationSpace, TimeFeature,
};
pub use reward::error::RewardError;
pub use reward::history::ReturnHistory;
pub use reward::multi_objective::MultiObjectiveReward;
pub use reward::pnl::PnLReward;
pub use reward::scaled::ScaledReward;
pub use reward::sharpe::{RiskAdjustedType, SharpeReward};
pub use reward::{
    RewardFn, compute_cumulative_return, compute_returns, create_reward_fn, default_multi_objective,
};
pub use vec_env::{
    AsyncVecEnv, BasicEnvFactory, EnvFactory, SyncVecEnv, VecEnvError, VecEnvStatistics,
};
