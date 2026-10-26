//! 观测空间核心类型
//!
//! 定义特征配置、市场状态、观测向量、BoxSpace 等基础数据结构。

use serde::{Deserialize, Serialize};

use crate::observation::error::ObservationError;
use crate::observation::normalizer::{RunningStats, make_normalizer};

// ── 时间特征 ──────────────────────────────────────────────

/// 时间维度的特征来源
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TimeFeature {
    /// 一天内的分钟数（0-395，US 股票交易时段）
    MinuteOfDay,
    /// 星期几（0-4，0 = 周一）
    DayOfWeek,
    /// 距离收盘的分钟数
    MinutesToClose,
    /// 正弦编码的时间周期
    SinCycle {
        /// 周期（分钟）
        period: usize,
    },
    /// 余弦编码的时间周期
    CosCycle {
        /// 周期（分钟）
        period: usize,
    },
}

// ── 聚合类型 ──────────────────────────────────────────────

/// 窗口聚合类型
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AggregationType {
    /// 算术平均
    Mean,
    /// 标准差
    Std,
    /// 最大值
    Max,
    /// 最小值
    Min,
    /// 最新值（`history` 最新在前 ⇒ 窗口里距当前最近的一根）
    Last,
    /// 偏度
    Skew,
    /// 峰度
    Kurtosis,
}

// ── 特征来源 ──────────────────────────────────────────────

/// 单个特征的来源与计算方式
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FeatureSource {
    /// 原始价格字段：`open` / `high` / `low` / `close` / `last` / `bid` / `ask` / `spread`
    PriceField(String),
    /// 成交量字段
    VolumeField(String),
    /// 持仓信息
    PositionField(String),
    /// 时间特征
    TimeField(TimeFeature),
    /// 衍生特征（由其他特征计算）
    Derived {
        /// 输入特征名列表
        inputs: Vec<String>,
        /// 表达式（如 `close / ema(close, 20) - 1.0`）
        expr: String,
    },
    /// 窗口聚合特征
    WindowAgg {
        /// 内部特征来源
        source: Box<FeatureSource>,
        /// 聚合方式
        agg: AggregationType,
        /// 窗口大小
        window: usize,
    },
}

// ── 归一化类型 ────────────────────────────────────────────

/// 归一化策略
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NormalizerType {
    /// Z-score: `(x - mean) / std`，保留历史统计量
    ZScore,
    /// Min-Max: `(x - min) / (max - min)` → `[0, 1]`
    MinMax,
    /// Robust: `(x - median) / IQR`，抗异常值
    Robust,
    /// 无归一化
    None,
}

// ── 特征配置 ──────────────────────────────────────────────

/// 单个特征的完整配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureConfig {
    /// 特征名称（观测向量中的列名）
    pub name: String,
    /// 特征来源
    pub source: FeatureSource,
    /// 归一化策略
    pub normalizer: NormalizerType,
    /// clip 范围（`Some((lo, hi))` 表示归一化后再 clip）
    pub clip_range: Option<(f64, f64)>,
}

// ── BoxSpace (Gymnasium 兼容) ────────────────────────────

/// Gymnasium `BoxSpace` 的 Rust 表示
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoxSpace {
    /// 形状（多维张量）
    pub shape: Vec<usize>,
    /// 各维下界
    pub low: Vec<f64>,
    /// 各维上界
    pub high: Vec<f64>,
    /// 数据类型
    pub dtype: DType,
}

/// 浮点数据类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DType {
    /// 32 位浮点
    Float32,
    /// 64 位浮点
    Float64,
}

// ── MarketState ───────────────────────────────────────────

/// 喂入观测构造器的原始市场数据快照
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarketState {
    /// 时间戳（毫秒）
    pub timestamp: u64,
    /// 标的代码
    pub symbol: String,
    /// K线 open
    pub open: f64,
    /// K线 high
    pub high: f64,
    /// K线 low
    pub low: f64,
    /// K线 close
    pub close: f64,
    /// 最新成交价
    pub last_price: f64,
    /// 成交量
    pub volume: f64,
    /// 买一价（`None` 表示无买单）
    pub bid: Option<f64>,
    /// 卖一价
    pub ask: Option<f64>,
    /// 买卖价差
    pub spread: Option<f64>,
    /// 当前持仓（正数 = 多头，负数 = 空头）
    pub position: f64,
    /// 可用资金
    pub cash: f64,
    /// 组合总市值
    pub portfolio_value: f64,
    /// 浮动盈亏
    pub unrealized_pnl: f64,
    /// 已实现盈亏
    pub realized_pnl: f64,
}

// ── Observation ───────────────────────────────────────────

/// 从 `MarketState` 构建的原始观测
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// 特征值（一维向量，shape = `[num_features]`）
    pub features: Vec<f64>,
    /// 各维度对应的特征名（与 `features` 等长）
    pub feature_names: Vec<String>,
    /// 时间戳
    pub timestamp: Option<u64>,
}

impl Observation {
    /// 构造空观测
    pub fn empty() -> Self {
        Self {
            features: Vec::new(),
            feature_names: Vec::new(),
            timestamp: None,
        }
    }

    /// 形状（一维）
    pub fn shape(&self) -> Vec<usize> {
        vec![self.features.len()]
    }

    /// 转换为 numpy-compatible 的 f32 数组
    pub fn as_f32_slice(&self) -> Vec<f32> {
        self.features.iter().map(|&x| x as f32).collect()
    }
}

// ── ObservationSpace trait ────────────────────────────────

/// 观测空间 trait：把市场状态映射为特征向量
pub trait ObservationSpace: Send + Sync {
    /// 返回张量形状（一维长度）
    fn shape(&self) -> Vec<usize>;
    /// 各维下界
    fn low(&self) -> Vec<f64>;
    /// 各维上界
    fn high(&self) -> Vec<f64>;
    /// Gymnasium `BoxSpace` 表示
    fn gymnasium_box(&self) -> BoxSpace;
    /// 从 `MarketState` 与历史窗口构建观测
    fn build(
        &self,
        state: &MarketState,
        history: &[MarketState],
    ) -> Result<Observation, ObservationError>;
    /// 训练专用：先按当前统计量归一化，再用原始特征值更新统计量
    ///
    /// 推理侧必须继续用 [`Self::build`]（只读），否则同一次运行会随 step 改变观测口径。
    /// 默认实现委托 [`Self::build`]：没有归一化状态的实现没有可累积的东西。
    ///
    /// # 破坏性语义
    ///
    /// 在这个入口出现之前，训练路径只走只读 `build`，Z-Score 统计量恒为空，
    /// 喂给模型的观测全是 0。**此前训练的模型是在全零观测上训的，不可与本次之后的
    /// 产物比较、也不可用同一套权重做基线。**
    fn build_and_update(
        &mut self,
        state: &MarketState,
        history: &[MarketState],
    ) -> Result<Observation, ObservationError> {
        self.build(state, history)
    }
    /// 所有特征名（按 `features` 顺序）
    fn feature_names(&self) -> Vec<String>;
    /// 观测窗口完整所需的最少历史根数（含当前 bar）
    ///
    /// 调用方（内核 RL host / 训练环境）据此保证喂给 [`Self::build`] 的历史
    /// 至少有这么多根——不足时纯 PriceField 特征也会因产出短观测而报错，
    /// 预热期必须等待而不是降级。无默认实现：trait 本身不知道窗口概念，
    /// 唯一合理的来源是具体实现的空间配置。
    fn required_history_depth(&self) -> usize;
    /// 特征维度总数（`num_features × window_size`）
    fn num_features(&self) -> usize;
    /// 归一化运行时统计量（仅带归一化的实现返回 `Some`）
    ///
    /// 顺序与特征配置逐条对齐（一特征一条统计）。训练结束时调用方把它落进
    /// 模型旁车，推理侧再灌回，才不至于让 Z-Score 在推理期因空统计恒为 0。
    fn normalizer_stats(&self) -> Option<&[RunningStats]> {
        None
    }
    /// 灌回归一化运行时统计量（训练恢复 / 内核复现用）
    ///
    /// 默认拒绝：不持有归一化状态的实现没有可灌回的位置。实现方必须校验
    /// 长度与特征数一致——观测组装按下标取统计，长度不符就会越界。
    fn set_normalizer_stats(&mut self, _stats: Vec<RunningStats>) -> Result<(), ObservationError> {
        Err(ObservationError::NormalizationFailed {
            reason: "observation space does not support normalizer stats".to_string(),
        })
    }
}

// ── 内部辅助：从历史 + 当前状态提取归一化后的特征值 ──────

/// 提取单个特征在指定时间步上的值
pub(crate) fn extract_feature_value(
    source: &FeatureSource,
    state: &MarketState,
    history: &[MarketState],
) -> Result<f64, ObservationError> {
    match source {
        FeatureSource::PriceField(field) => match field.as_str() {
            "open" => Ok(state.open),
            "high" => Ok(state.high),
            "low" => Ok(state.low),
            "close" => Ok(state.close),
            "last" => Ok(state.last_price),
            "bid" => Ok(state.bid.unwrap_or(0.0)),
            "ask" => Ok(state.ask.unwrap_or(0.0)),
            "spread" => Ok(state.spread.unwrap_or(0.0)),
            _ => Err(ObservationError::FeatureNotFound {
                feature: field.clone(),
            }),
        },
        FeatureSource::VolumeField(field) => match field.as_str() {
            "volume" => Ok(state.volume),
            _ => Err(ObservationError::FeatureNotFound {
                feature: field.clone(),
            }),
        },
        FeatureSource::PositionField(field) => match field.as_str() {
            "position" => Ok(state.position),
            "cash" => Ok(state.cash),
            "portfolio_value" => Ok(state.portfolio_value),
            "unrealized_pnl" => Ok(state.unrealized_pnl),
            "realized_pnl" => Ok(state.realized_pnl),
            _ => Err(ObservationError::FeatureNotFound {
                feature: field.clone(),
            }),
        },
        FeatureSource::TimeField(tf) => {
            let minutes = (state.timestamp / 60_000) % 396;
            match tf {
                TimeFeature::MinuteOfDay => Ok(minutes as f64),
                TimeFeature::DayOfWeek => Ok(((state.timestamp / 86_400_000 + 4) % 7) as f64),
                TimeFeature::MinutesToClose => Ok((395 - minutes) as f64),
                TimeFeature::SinCycle { period } => {
                    Ok((2.0 * std::f64::consts::PI * minutes as f64 / *period as f64).sin())
                }
                TimeFeature::CosCycle { period } => {
                    Ok((2.0 * std::f64::consts::PI * minutes as f64 / *period as f64).cos())
                }
            }
        }
        FeatureSource::Derived { inputs, expr } => {
            // 简化实现：仅识别 `last - open` / `last - close` 形式
            let _ = (inputs, expr);
            Ok(state.last_price - state.open)
        }
        FeatureSource::WindowAgg {
            source: inner,
            agg,
            window,
        } => {
            if history.len() < *window {
                return Err(ObservationError::InsufficientData {
                    needed: *window,
                    have: history.len(),
                });
            }
            // `history` 最新在前：最新的 `window` 根就在 `history[..window]`
            let values: Vec<f64> = history[..*window]
                .iter()
                .map(|s| extract_feature_value(inner, s, &[]))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(match agg {
                AggregationType::Mean => values.iter().sum::<f64>() / values.len() as f64,
                AggregationType::Std => {
                    let mean = values.iter().sum::<f64>() / values.len() as f64;
                    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>()
                        / values.len() as f64;
                    var.sqrt()
                }
                AggregationType::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                AggregationType::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
                // Last = 距当前最近的一根；窗口最新在前，故取首元素而非末元素
                AggregationType::Last => values.first().copied().unwrap_or(0.0),
                AggregationType::Skew => {
                    let n = values.len() as f64;
                    let mean = values.iter().sum::<f64>() / n;
                    let std = (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
                    if std == 0.0 {
                        0.0
                    } else {
                        values
                            .iter()
                            .map(|v| ((v - mean) / std).powi(3))
                            .sum::<f64>()
                            / n
                    }
                }
                AggregationType::Kurtosis => {
                    let n = values.len() as f64;
                    let mean = values.iter().sum::<f64>() / n;
                    let std = (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
                    if std == 0.0 {
                        0.0
                    } else {
                        values
                            .iter()
                            .map(|v| ((v - mean) / std).powi(4))
                            .sum::<f64>()
                            / n
                            - 3.0
                    }
                }
            })
        }
    }
}

// ── 内部辅助：归一化 + clip ───────────────────────────────

/// 归一化 + clip 单个值
pub(crate) fn normalize_and_clip(
    raw: f64,
    normalizer_type: &NormalizerType,
    stats: &RunningStats,
    clip_range: Option<(f64, f64)>,
) -> f64 {
    let normalizer = make_normalizer(normalizer_type);
    let normalized = normalizer.normalize(raw, stats);
    match clip_range {
        Some((lo, hi)) => normalized.clamp(lo, hi),
        None => normalized,
    }
}
