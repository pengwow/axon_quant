//! 环境工厂：把"如何创建一个 `TradingEnv`"抽象成可克隆的描述
//!
//! `TradingEnv` 内部包含 `Box<dyn ObservationSpace>` / `Box<dyn RewardFn>`，
//! 这些 trait object 不可 `Clone`。为了在 N 个线程里独立构造 N 个环境实例，
//! 我们引入 `EnvFactory` trait：把"建环境的配方"作为可克隆数据传给每个 worker。
//!
//! 两个内置实现：
//! - [`BasicEnvFactory`]：使用 [`EnvConfig`] 共享配置，每个 worker 偏移 seed
//! - 自定义实现：用户可以传入"按 env_id 切片市场数据"等更复杂的配方

use crate::action::types::ActionSpace;
use crate::env::EnvResult;
use crate::env::config::EnvConfig;
use crate::env::trading_env::TradingEnv;
use crate::env::types::MarketBar;
use crate::observation::space::DefaultObservationSpace;
use crate::observation::types::{FeatureConfig, NormalizerType};
use crate::reward::RewardFn;
use crate::reward::pnl::PnLReward;

/// 环境工厂 trait：把"如何创建一个 `TradingEnv`"抽象成可克隆的配方
///
/// 实现必须是 `Send + Sync`，因为它们会被多个 worker 线程同时调用。
pub trait EnvFactory: Send + Sync {
    /// 给定环境索引 `env_id`，构造一个全新的 `TradingEnv`
    ///
    /// **每次调用都必须返回独立的实例**，因为 VecEnv 中的不同环境不应共享状态。
    fn build_env(&self, env_id: usize) -> EnvResult<TradingEnv>;
}

// ── 内置工厂：BasicEnvFactory ─────────────────────────────────

/// 基础环境工厂：所有 worker 共享同一组参数
///
/// - `config` 共享配置；worker 会把 `seed` 替换为 `seed + env_id` 以保证多样性
/// - `action_space` 共享动作空间
/// - `features` 共享特征配置（用于默认观测空间）
/// - `market_data` 共享同一份 K 线；每个 worker 看到完全相同的数据
/// - `reward_kind` 奖励函数类型：默认 "pnl"
#[derive(Clone)]
pub struct BasicEnvFactory {
    /// 环境配置
    pub config: EnvConfig,
    /// 动作空间
    pub action_space: ActionSpace,
    /// 默认观测空间的特征配置
    pub features: Vec<FeatureConfig>,
    /// 共享市场数据
    pub market_data: Vec<MarketBar>,
    /// 奖励函数名（"pnl" / "sharpe" / "sortino"），默认 "pnl"
    pub reward_kind: String,
}

impl std::fmt::Debug for BasicEnvFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasicEnvFactory")
            .field("config", &self.config)
            .field("action_space", &self.action_space)
            .field("features_len", &self.features.len())
            .field("market_data_len", &self.market_data.len())
            .field("reward_kind", &self.reward_kind)
            .finish()
    }
}

impl BasicEnvFactory {
    /// 构造基础工厂：默认 PnL 奖励、close+volume 特征
    ///
    /// 适用于快速原型和大多数用例。如需自定义奖励/特征，
    /// 请使用 [`BasicEnvFactory::with_reward_kind`] 或自定义实现 [`EnvFactory`]。
    pub fn new(config: EnvConfig, action_space: ActionSpace, market_data: Vec<MarketBar>) -> Self {
        let features = vec![
            FeatureConfig {
                name: "close".to_string(),
                source: crate::observation::types::FeatureSource::PriceField("close".to_string()),
                normalizer: NormalizerType::ZScore,
                clip_range: None,
            },
            FeatureConfig {
                name: "volume".to_string(),
                source: crate::observation::types::FeatureSource::VolumeField("volume".to_string()),
                normalizer: NormalizerType::None,
                clip_range: None,
            },
        ];
        Self {
            config,
            action_space,
            features,
            market_data,
            reward_kind: "pnl".to_string(),
        }
    }

    /// 设置奖励函数类型
    pub fn with_reward_kind(mut self, kind: impl Into<String>) -> Self {
        self.reward_kind = kind.into();
        self
    }

    /// 设置特征配置
    pub fn with_features(mut self, features: Vec<FeatureConfig>) -> Self {
        self.features = features;
        self
    }
}

impl EnvFactory for BasicEnvFactory {
    fn build_env(&self, env_id: usize) -> EnvResult<TradingEnv> {
        // 偏移 seed 以保证多样性
        let config = EnvConfig {
            seed: self.config.seed.map(|s| s.wrapping_add(env_id as u64)),
            ..self.config.clone()
        };

        // 构造默认观测空间
        let observation_space = DefaultObservationSpace::new(1, self.features.clone())
            .map_err(|e| crate::env::error::EnvError::ObservationError(e.to_string()))?;

        // 构造奖励函数
        let reward_fn: Box<dyn RewardFn> = match self.reward_kind.as_str() {
            "pnl" => Box::new(PnLReward::default()),
            other => {
                // 暂时仅支持 pnl；其他类型返回清晰错误
                return Err(crate::env::error::EnvError::InvalidAction(format!(
                    "unsupported reward kind in factory: {other}"
                )));
            }
        };

        TradingEnv::new(
            config,
            self.action_space.clone(),
            Box::new(observation_space),
            reward_fn,
            self.market_data.clone(),
        )
    }
}
