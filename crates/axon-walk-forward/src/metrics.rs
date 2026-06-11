//! Walk-Forward 评估指标与结果

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::WalkForwardConfig;
pub use crate::split::FoldSplit;

/// In-Sample 指标
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ISMetrics {
    /// 总收益率
    pub total_return: f64,
    /// 夏普比率
    pub sharpe_ratio: f64,
    /// 最大回撤（负数或 0）
    pub max_drawdown: f64,
    /// 胜率（0~1）
    pub win_rate: f64,
    /// 盈亏比
    pub profit_factor: f64,
}

impl Default for ISMetrics {
    fn default() -> Self {
        Self {
            total_return: 0.0,
            sharpe_ratio: 0.0,
            max_drawdown: 0.0,
            win_rate: 0.0,
            profit_factor: 0.0,
        }
    }
}

/// Out-of-Sample 指标
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OOSMetrics {
    /// 总收益率
    pub total_return: f64,
    /// 夏普比率
    pub sharpe_ratio: f64,
    /// 最大回撤（负数或 0）
    pub max_drawdown: f64,
    /// 胜率（0~1）
    pub win_rate: f64,
    /// 盈亏比
    pub profit_factor: f64,
    /// Calmar 比率（年化收益 / 最大回撤绝对值）
    pub calmar_ratio: f64,
}

impl Default for OOSMetrics {
    fn default() -> Self {
        Self {
            total_return: 0.0,
            sharpe_ratio: 0.0,
            max_drawdown: 0.0,
            win_rate: 0.0,
            profit_factor: 0.0,
            calmar_ratio: 0.0,
        }
    }
}

/// 单个 fold 的评估结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoldResult {
    /// fold 序号
    pub fold_id: usize,
    /// 分割信息
    pub split: FoldSplit,
    /// In-Sample 指标
    pub is_metrics: ISMetrics,
    /// Out-of-Sample 指标
    pub oos_metrics: OOSMetrics,
    /// 过拟合比率（IS / OOS，> 1 表示可能过拟合）
    pub overfit_ratio: f64,
}

impl FoldResult {
    /// 创建 fold 结果
    pub fn new(
        fold_id: usize,
        split: FoldSplit,
        is_metrics: ISMetrics,
        oos_metrics: OOSMetrics,
    ) -> Self {
        let overfit_ratio = if oos_metrics.total_return.abs() > 1e-9 {
            is_metrics.total_return / oos_metrics.total_return
        } else {
            f64::INFINITY
        };
        Self {
            fold_id,
            split,
            is_metrics,
            oos_metrics,
            overfit_ratio,
        }
    }
}

/// 汇总指标（所有 fold 的聚合）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AggregatedMetrics {
    /// OOS 平均收益
    pub mean_oos_return: f64,
    /// OOS 收益标准差
    pub std_oos_return: f64,
    /// OOS 平均夏普
    pub mean_oos_sharpe: f64,
    /// OOS 夏普标准差
    pub std_oos_sharpe: f64,
    /// OOS 中位收益
    pub median_oos_return: f64,
    /// 最差 fold 收益
    pub worst_fold_return: f64,
    /// 最佳 fold 收益
    pub best_fold_return: f64,
    /// 盈利 fold 占比
    pub pct_profitable_folds: f64,
}

/// 稳定性指标
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StabilityMetrics {
    /// Sharpe of Sharpe：Sharpe 比率的标准误倒数
    pub sharpe_of_sharpe: f64,
    /// fold 间收益自相关
    pub return_autocorrelation: f64,
    /// Deflated Sharpe Ratio（多重比较修正）
    pub deflated_sharpe: f64,
    /// 下一 fold 亏损概率
    pub probability_of_loss: f64,
}

/// Walk-Forward 完整结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkForwardResult {
    /// 配置
    pub config: WalkForwardConfig,
    /// 所有 fold 结果
    pub folds: Vec<FoldResult>,
    /// 汇总指标
    pub aggregated: AggregatedMetrics,
    /// 稳定性指标
    pub stability: StabilityMetrics,
}

impl WalkForwardResult {
    /// 创建空结果
    pub fn empty(config: WalkForwardConfig) -> Self {
        Self {
            config,
            folds: Vec::new(),
            aggregated: AggregatedMetrics::default(),
            stability: StabilityMetrics::default(),
        }
    }

    /// 完成的 fold 数
    pub fn n_folds(&self) -> usize {
        self.folds.len()
    }

    /// 自定义字段（如训练时长、checkpoint 路径等）
    pub fn extras(&self) -> &HashMap<String, serde_json::Value> {
        static EMPTY: std::sync::OnceLock<HashMap<String, serde_json::Value>> =
            std::sync::OnceLock::new();
        EMPTY.get_or_init(HashMap::new)
    }
}

/// 泄漏检测报告
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeakageCheck {
    /// 是否检测到泄漏
    pub has_leakage: bool,
    /// 泄漏的索引对
    pub leaked_indices: Vec<(usize, usize)>,
    /// 详细描述
    pub details: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_split() -> FoldSplit {
        FoldSplit {
            fold_id: 0,
            train_start: 0,
            train_end: 100,
            validation_start: 100,
            validation_end: 100,
            test_start: 100,
            test_end: 150,
        }
    }

    #[test]
    fn test_is_metrics_default() {
        let m = ISMetrics::default();
        assert_eq!(m.total_return, 0.0);
    }

    #[test]
    fn test_oos_metrics_default() {
        let m = OOSMetrics::default();
        assert_eq!(m.calmar_ratio, 0.0);
    }

    #[test]
    fn test_fold_result_overfit_ratio() {
        let is_m = ISMetrics {
            total_return: 0.20,
            ..ISMetrics::default()
        };
        let oos_m = OOSMetrics {
            total_return: 0.10,
            ..OOSMetrics::default()
        };
        let f = FoldResult::new(0, make_split(), is_m, oos_m);
        assert!((f.overfit_ratio - 2.0).abs() < 1e-9);
    }

    #[test]
    fn test_fold_result_overfit_ratio_zero_oos() {
        let is_m = ISMetrics {
            total_return: 0.20,
            ..ISMetrics::default()
        };
        let oos_m = OOSMetrics {
            total_return: 0.0,
            ..OOSMetrics::default()
        };
        let f = FoldResult::new(0, make_split(), is_m, oos_m);
        assert!(f.overfit_ratio.is_infinite());
    }

    #[test]
    fn test_aggregated_default() {
        let a = AggregatedMetrics::default();
        assert_eq!(a.mean_oos_return, 0.0);
        assert_eq!(a.pct_profitable_folds, 0.0);
    }

    #[test]
    fn test_stability_default() {
        let s = StabilityMetrics::default();
        assert_eq!(s.deflated_sharpe, 0.0);
    }

    #[test]
    fn test_walk_forward_result_empty() {
        let cfg = WalkForwardConfig::expanding(100, 50, 50);
        let r = WalkForwardResult::empty(cfg);
        assert_eq!(r.n_folds(), 0);
    }

    #[test]
    fn test_leakage_check_struct() {
        let l = LeakageCheck {
            has_leakage: true,
            leaked_indices: vec![(1, 1)],
            details: "test".to_string(),
        };
        assert!(l.has_leakage);
        assert_eq!(l.leaked_indices.len(), 1);
    }
}
