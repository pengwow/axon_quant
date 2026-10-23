//! 观测组装规格 —— 训练与推理共用这一份实现
//!
//! `DefaultObservationSpace`（Gymnasium 训练/评估路径）与内核 `RLStrategyHost`
//! （推理路径）都必须走本模块的组装逻辑，避免特征顺序或归一化统计漂移导致
//! 模型静默失效（统一内核设计 6.4 第 1 条）。
//!
//! # 输入约定
//!
//! [`ObservationInput::history`] 是**最新在前**的已收市窗口（`history[0]` 是当前
//! bar）。这与 `TradingEnv::build_observation` 收集历史的顺序一致：那边把
//! `(start..=current_step).rev()` 展平后交给观测空间，最新 bar 落在索引 0。
//! 顺序一旦颠倒，每个 `tN` 槽位装的特征就会错位，模型输入全变。

use serde::{Deserialize, Serialize};

use crate::action::state::PortfolioState;
use crate::env::types::MarketBar;
use crate::observation::error::{ObservationError, validate_observation_space};
use crate::observation::normalizer::RunningStats;
use crate::observation::types::{
    FeatureConfig, MarketState, Observation, extract_feature_value, normalize_and_clip,
};

/// 默认规格版本
///
/// 动作语义切到 `target_ratio ∈ [-1, 1]` 后训练产物必须升版（设计 6.4 第 2 条），
/// 调用方构造模型时显式传入自己的版本；这里只提供未声明时的兜底值。
pub const DEFAULT_SPEC_VERSION: &str = "v1";

/// 归一化运行时规格
///
/// 归一化**策略**与 clip 范围仍由 [`FeatureConfig`] 持有（那是静态规格）；
/// 本结构只承载随数据变化、且训练结束必须与模型一起落盘的 Welford 统计量
/// （设计 6.4 的「产物三元组」）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NormalizerConfig {
    /// 逐特征运行时统计，顺序与 [`ObservationSpec::features`] 严格对齐
    pub stats: Vec<RunningStats>,
}

impl NormalizerConfig {
    /// 为 `n` 个特征构造全空的统计量
    pub fn new(n: usize) -> Self {
        Self {
            stats: vec![RunningStats::new(); n],
        }
    }

    /// 重置所有统计量
    pub fn reset(&mut self) {
        for stats in &mut self.stats {
            stats.reset();
        }
    }
}

/// 观测组装规格
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationSpec {
    /// 窗口大小（每个特征保留最近 N 根 bar）
    pub window_size: usize,
    /// 特征配置列表
    pub features: Vec<FeatureConfig>,
    /// 归一化运行时统计
    pub normalizer: NormalizerConfig,
    /// 规格版本（模型产物三元组之一）
    pub spec_version: String,
}

impl ObservationSpec {
    /// 构造规格（统计量全空，对应训练冷启动）
    pub fn new(window_size: usize, features: Vec<FeatureConfig>) -> Result<Self, ObservationError> {
        validate_observation_space(&features, window_size)?;
        let normalizer = NormalizerConfig::new(features.len());
        Ok(Self {
            window_size,
            features,
            normalizer,
            spec_version: DEFAULT_SPEC_VERSION.to_string(),
        })
    }

    /// 指定规格版本
    pub fn with_version(mut self, spec_version: impl Into<String>) -> Self {
        self.spec_version = spec_version.into();
        self
    }

    /// 观测向量长度（一维）
    pub fn shape(&self) -> Vec<usize> {
        vec![self.features.len() * self.window_size]
    }

    /// 特征维度总数（`num_features × window_size`）
    pub fn num_features(&self) -> usize {
        self.features.len() * self.window_size
    }

    /// 从 `MarketState` 与历史窗口构建完整观测
    ///
    /// 这是**唯一**的特征组装实现：`DefaultObservationSpace::build` 也委托到这里。
    /// `state` 目前只贡献观测的时间戳（与历史实现逐字一致），特征值一律从
    /// `history` 里取。
    pub fn build_observation(
        &self,
        state: &MarketState,
        history: &[MarketState],
    ) -> Result<Observation, ObservationError> {
        let num_features = self.features.len();
        let window = self.window_size;
        let mut features = Vec::with_capacity(num_features * window);
        let mut feature_names = Vec::with_capacity(num_features * window);

        // 起始索引：保留最多 window 个历史点
        let start = if history.len() >= window {
            history.len() - window
        } else {
            0
        };

        for (feat_idx, feat_config) in self.features.iter().enumerate() {
            for t in start..history.len() {
                let raw = extract_feature_value(&feat_config.source, &history[t], &history[..t])?;
                let normalized = normalize_and_clip(
                    raw,
                    &feat_config.normalizer,
                    &self.normalizer.stats[feat_idx],
                    feat_config.clip_range,
                );
                features.push(normalized);
                feature_names.push(format!("{}_t{}", feat_config.name, history.len() - t - 1));
            }
        }

        Ok(Observation {
            features,
            feature_names,
            timestamp: Some(state.timestamp),
        })
    }

    /// 从内核侧输入构建观测向量
    ///
    /// 失败时保留错误（`InsufficientData` / `FeatureNotFound` 等都可能来自
    /// 配置错误），供调用方决定降级策略；内核 `RLStrategyHost` 用本方法并在
    /// 失败时选择 Hold，不把坏观测喂给模型。
    pub fn try_build(&self, input: &ObservationInput<'_>) -> Result<Vec<f64>, ObservationError> {
        let history: Vec<MarketState> = input.history.iter().map(market_state_of).collect();
        // `input.bar` 是当前（已收市）bar；组合字段只挂在这里，历史条目保持
        // 纯行情（与 `TradingEnv` 收集历史的口径一致）。
        let state = MarketState {
            position: input.portfolio.position,
            cash: input.portfolio.cash,
            portfolio_value: input.portfolio.portfolio_value,
            unrealized_pnl: input.portfolio.unrealized_pnl,
            ..market_state_of(input.bar)
        };
        Ok(self.build_observation(&state, &history)?.features)
    }

    /// `try_build` 的不可失败封装
    ///
    /// 构建失败时返回 `shape` 长度的全零向量并记一条 warn。零向量不是好观测，
    /// 因此**关键路径（推理）应直接用 [`Self::try_build`]**，让 Host 显式选择
    /// Hold；本方法只服务那些签名上无法返回 Result 的调用方。
    pub fn build(&self, input: &ObservationInput<'_>) -> Vec<f64> {
        match self.try_build(input) {
            Ok(features) => features,
            Err(err) => {
                tracing::warn!(error = %err, "observation spec build failed, fallback to zeros");
                vec![0.0; self.shape()[0]]
            }
        }
    }
}

/// 观测组装输入
///
/// 用 `axon-rl` 自己的 [`MarketBar`]（毫秒时间戳、无 symbol）而不是
/// `axon_core::Bar`（纳秒时间戳）：`ObservationSpec` 住在 `axon-rl`，内核侧
/// 只在 bar 边界做一次单位换算，避免观测层再引入一套时间口径。
#[derive(Debug)]
pub struct ObservationInput<'a> {
    /// 当前（已收市）bar
    pub bar: &'a MarketBar,
    /// 已收市窗口，**最新在前**（`history[0]` = 当前 bar）
    pub history: &'a [MarketBar],
    /// 组合状态（持仓 / 现金 / 权益）
    pub portfolio: &'a PortfolioState,
}

/// `MarketBar` → 纯行情 `MarketState`
///
/// `MarketBar` 没有 symbol，这里留空：现有 `FeatureSource` 都不读 symbol。
fn market_state_of(bar: &MarketBar) -> MarketState {
    MarketState {
        timestamp: bar.timestamp,
        open: bar.open,
        high: bar.high,
        low: bar.low,
        close: bar.close,
        last_price: bar.close,
        volume: bar.volume,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::space::DefaultObservationSpace;
    use crate::observation::types::{
        AggregationType, FeatureSource, NormalizerType, ObservationSpace,
    };

    fn feature(name: &str, source: FeatureSource, normalizer: NormalizerType) -> FeatureConfig {
        FeatureConfig {
            name: name.to_string(),
            source,
            normalizer,
            clip_range: Some((-5.0, 5.0)),
        }
    }

    /// 一份覆盖三条不同特征路径的规格：价格 / 成交量 / 持仓 / 窗口聚合
    fn features() -> Vec<FeatureConfig> {
        vec![
            feature(
                "close",
                FeatureSource::PriceField("close".into()),
                NormalizerType::ZScore,
            ),
            feature(
                "volume",
                FeatureSource::VolumeField("volume".into()),
                NormalizerType::None,
            ),
            feature(
                "position",
                FeatureSource::PositionField("position".into()),
                NormalizerType::None,
            ),
            feature(
                "close_mean",
                FeatureSource::WindowAgg {
                    source: Box::new(FeatureSource::PriceField("close".into())),
                    agg: AggregationType::Mean,
                    window: 2,
                },
                NormalizerType::MinMax,
            ),
        ]
    }

    fn bar(ts: u64, close: f64) -> MarketBar {
        MarketBar::new(
            ts,
            close - 0.5,
            close + 0.5,
            close - 1.0,
            close,
            100.0 + close,
        )
    }

    /// 同一输入序列上 `ObservationSpec::build` 与 `DefaultObservationSpace::build`
    /// 必须逐位相等（`assert_eq!` 浮点向量，不允许容差）
    #[test]
    fn test_spec_matches_default_observation_space_bitwise() {
        let window = 3;
        let mut spec = ObservationSpec::new(window, features()).unwrap();
        let mut space = DefaultObservationSpace::new(window, features()).unwrap();
        // 两侧灌同一组运行时统计，让 Z-Score / Min-Max 真正参与计算，
        // 否则空统计下归一化恒为 0 / 0.5，逐位相等没有力度
        for idx in 0..features().len() {
            for value in [95.0, 100.0, 105.0] {
                spec.normalizer.stats[idx].update(value);
                space.running_stats_mut(idx).update(value);
            }
        }

        // 6 根 bar：窗口聚合要求 t >= window，因此历史长度至少 2 × window
        let bars: Vec<MarketBar> = (0..6u64)
            .map(|i| bar(i * 60_000, 100.0 + i as f64))
            .collect();
        // 最新在前，与 TradingEnv 收集历史的顺序一致
        let mut window_bars: Vec<MarketBar> = bars.clone();
        window_bars.reverse();

        // 历史条目只带行情（组合字段为 0），与 TradingEnv 的口径一致
        let history: Vec<MarketState> = window_bars.iter().map(market_state_of).collect();
        let portfolio = PortfolioState {
            position: 1.25,
            cash: 5_000.0,
            portfolio_value: 6_250.0,
            unrealized_pnl: 250.0,
            last_price: bars.last().unwrap().close,
            ..Default::default()
        };
        // 当前 bar 对应 `history[0]`
        let state = MarketState {
            position: portfolio.position,
            cash: portfolio.cash,
            portfolio_value: portfolio.portfolio_value,
            unrealized_pnl: portfolio.unrealized_pnl,
            last_price: portfolio.last_price,
            ..market_state_of(&window_bars[0])
        };

        let expected = space.build(&state, &history).unwrap();
        let input = ObservationInput {
            bar: &window_bars[0],
            history: &window_bars,
            portfolio: &portfolio,
        };
        let actual = spec.build(&input);

        assert_eq!(
            actual, expected.features,
            "ObservationSpec 与 DefaultObservationSpace 必须逐位相同"
        );
        // 断言向量不是退化全零：证明归一化统计确实参与了计算
        assert!(
            actual.iter().any(|value| *value != 0.0),
            "观测向量退化，逐位相等测试失去力度: {actual:?}"
        );
        // 逐位相等也覆盖了「历史窗口只取最近 window 根」的截断语义
        assert_eq!(actual.len(), features().len() * window);
    }

    /// 历史短于 window 时两者都必须产出短向量（不是补零）
    #[test]
    fn test_spec_matches_short_history() {
        let window = 5;
        let spec = ObservationSpec::new(window, vec![features()[1].clone()]).unwrap();
        let space = DefaultObservationSpace::new(window, vec![features()[1].clone()]).unwrap();

        let bars: Vec<MarketBar> = (0..2u64).map(|i| bar(i, 50.0 + i as f64)).collect();
        let history: Vec<MarketState> = bars.iter().map(market_state_of).collect();
        let portfolio = PortfolioState::default();

        let expected = space
            .build(&market_state_of(&bars[0]), &history)
            .unwrap()
            .features;
        let actual = spec.build(&ObservationInput {
            bar: &bars[0],
            history: &bars,
            portfolio: &portfolio,
        });
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 2, "短历史不补零");
    }

    #[test]
    fn test_spec_shape_and_version() {
        let spec = ObservationSpec::new(4, features())
            .unwrap()
            .with_version("2.0.0");
        assert_eq!(spec.shape(), vec![features().len() * 4]);
        assert_eq!(spec.num_features(), features().len() * 4);
        assert_eq!(spec.spec_version, "2.0.0");
    }

    #[test]
    fn test_spec_rejects_invalid_config() {
        assert!(ObservationSpec::new(0, features()).is_err());
        assert!(ObservationSpec::new(3, Vec::new()).is_err());
        let dup = vec![features()[0].clone(), features()[0].clone()];
        assert!(ObservationSpec::new(3, dup).is_err());
    }

    /// 配置错误（未知价格字段）走 `try_build` 报错，`build` 才降级为零向量
    #[test]
    fn test_spec_build_error_paths() {
        let bad = vec![feature(
            "nope",
            FeatureSource::PriceField("nonexistent".into()),
            NormalizerType::None,
        )];
        let spec = ObservationSpec::new(2, bad).unwrap();
        let bars = vec![bar(0, 100.0)];
        let portfolio = PortfolioState::default();
        let input = ObservationInput {
            bar: &bars[0],
            history: &bars,
            portfolio: &portfolio,
        };
        assert!(matches!(
            spec.try_build(&input),
            Err(ObservationError::FeatureNotFound { .. })
        ));
        assert_eq!(spec.build(&input), vec![0.0; 2], "降级为零向量");
    }
}
