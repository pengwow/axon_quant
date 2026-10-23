//! 默认观测空间实现
//!
//! 组合特征提取 + 归一化 + 窗口聚合，对外暴露 `ObservationSpace` trait。
//!
//! 组装逻辑**不再在本模块重复实现**：内部持有一份 [`ObservationSpec`]，所有
//! 计算都委托给它，保证 Gymnasium 训练/评估与内核推理是同一份代码。

use crate::observation::error::ObservationError;
use crate::observation::spec::ObservationSpec;
use crate::observation::types::{
    BoxSpace, DType, FeatureConfig, MarketState, Observation, ObservationSpace,
};

/// 默认观测空间：特征工程管线
///
/// 内部只有一个 [`ObservationSpec`]；运行时统计量挂在
/// `spec.normalizer.stats` 上。
pub struct DefaultObservationSpace {
    /// 唯一一份组装规格
    spec: ObservationSpec,
}

impl DefaultObservationSpace {
    /// 构造默认观测空间（Z-Score 归一化）
    pub fn new(window_size: usize, features: Vec<FeatureConfig>) -> Result<Self, ObservationError> {
        Ok(Self {
            spec: ObservationSpec::new(window_size, features)?,
        })
    }

    /// 观测组装规格（只读）
    pub fn spec(&self) -> &ObservationSpec {
        &self.spec
    }

    /// 获取某个特征的运行时统计量（仅测试可见）
    #[cfg(test)]
    pub fn running_stats(&self, idx: usize) -> &crate::observation::normalizer::RunningStats {
        &self.spec.normalizer.stats[idx]
    }

    /// 获取某个特征的运行时统计量（可变，仅测试可见）
    #[cfg(test)]
    pub fn running_stats_mut(
        &mut self,
        idx: usize,
    ) -> &mut crate::observation::normalizer::RunningStats {
        &mut self.spec.normalizer.stats[idx]
    }

    /// 重置所有运行时统计量
    pub fn reset_stats(&mut self) {
        self.spec.normalizer.reset();
    }
}

impl ObservationSpace for DefaultObservationSpace {
    fn shape(&self) -> Vec<usize> {
        self.spec.shape()
    }

    fn low(&self) -> Vec<f64> {
        vec![-5.0; self.spec.num_features()]
    }

    fn high(&self) -> Vec<f64> {
        vec![5.0; self.spec.num_features()]
    }

    fn gymnasium_box(&self) -> BoxSpace {
        BoxSpace {
            shape: self.shape(),
            low: self.low(),
            high: self.high(),
            dtype: DType::Float32,
        }
    }

    fn build(
        &self,
        state: &MarketState,
        history: &[MarketState],
    ) -> Result<Observation, ObservationError> {
        self.spec.build_observation(state, history)
    }

    fn feature_names(&self) -> Vec<String> {
        let mut names = Vec::with_capacity(self.spec.num_features());
        for feat in &self.spec.features {
            for t in 0..self.spec.window_size {
                names.push(format!("{}_t{}", feat.name, t));
            }
        }
        names
    }

    fn num_features(&self) -> usize {
        self.spec.num_features()
    }
}
