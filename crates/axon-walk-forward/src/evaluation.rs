//! Walk-Forward 指标聚合与稳定性分析

use crate::metrics::{AggregatedMetrics, FoldResult, StabilityMetrics};

/// 聚合所有 fold 的结果
///
/// Returns:
/// - `(aggregated, stability)` 元组
pub fn aggregate_folds(folds: &[FoldResult]) -> (AggregatedMetrics, StabilityMetrics) {
    if folds.is_empty() {
        return (AggregatedMetrics::default(), StabilityMetrics::default());
    }

    // 提取 OOS 指标
    let test_returns: Vec<f64> = folds.iter().map(|f| f.oos_metrics.total_return).collect();
    let test_sharpes: Vec<f64> = folds.iter().map(|f| f.oos_metrics.sharpe_ratio).collect();

    // 汇总
    let aggregated = AggregatedMetrics {
        mean_oos_return: mean(&test_returns),
        std_oos_return: stddev(&test_returns),
        mean_oos_sharpe: mean(&test_sharpes),
        std_oos_sharpe: stddev(&test_sharpes),
        median_oos_return: median(&test_returns),
        worst_fold_return: min_f64(&test_returns),
        best_fold_return: max_f64(&test_returns),
        pct_profitable_folds: if test_returns.is_empty() {
            0.0
        } else {
            test_returns.iter().filter(|&&r| r > 0.0).count() as f64 / test_returns.len() as f64
        },
    };

    // 稳定性
    let sharpe_of_sharpe = if test_sharpes.len() > 1 {
        let s = stddev(&test_sharpes);
        if s > 1e-9 {
            mean(&test_sharpes) / s
        } else {
            0.0
        }
    } else {
        0.0
    };

    let return_autocorrelation = if test_returns.len() > 2 {
        let prev: Vec<f64> = test_returns[..test_returns.len() - 1].to_vec();
        let curr: Vec<f64> = test_returns[1..].to_vec();
        pearson_correlation(&prev, &curr).unwrap_or(0.0)
    } else {
        0.0
    };

    let n_trials = test_sharpes.len();
    let sharpe_std = if n_trials > 1 {
        stddev(&test_sharpes)
    } else {
        1.0
    };
    let deflated = compute_deflated_sharpe(mean(&test_sharpes), n_trials, sharpe_std);

    let probability_of_loss = if test_returns.len() > 1 {
        let sd = stddev(&test_returns);
        if sd > 1e-9 {
            normal_cdf(0.0, mean(&test_returns), sd)
        } else {
            0.5
        }
    } else {
        0.5
    };

    let stability = StabilityMetrics {
        sharpe_of_sharpe,
        return_autocorrelation,
        deflated_sharpe: deflated,
        probability_of_loss,
    };

    (aggregated, stability)
}

/// Deflated Sharpe Ratio (Bailey & López de Prado, 2014)
///
/// 考虑多次试验的多重比较偏差。
pub fn compute_deflated_sharpe(observed_sharpe: f64, n_trials: usize, sharpe_std: f64) -> f64 {
    if sharpe_std.abs() < 1e-9 {
        return 0.0;
    }
    if n_trials == 0 {
        return 0.0;
    }

    // 期望最大 Sharpe（在无技能零假设下）
    let euler_gamma = 0.5772156649015329;
    let log_n = (n_trials as f64).ln().max(1.0);
    let sqrt_2_log_n = (2.0 * log_n).sqrt();
    let e_max =
        sqrt_2_log_n * (1.0 - euler_gamma / (2.0 * log_n)) + euler_gamma / (2.0 * sqrt_2_log_n);

    // 调整后 z-score → CDF
    let z = (observed_sharpe - e_max) / sharpe_std;
    normal_cdf(z, 0.0, 1.0)
}

// ── 辅助统计函数 ─────────────────────────────────────────

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

fn stddev(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    let var = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() as f64 - 1.0);
    var.sqrt()
}

fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    }
}

fn min_f64(xs: &[f64]) -> f64 {
    xs.iter().copied().fold(f64::INFINITY, f64::min)
}

fn max_f64(xs: &[f64]) -> f64 {
    xs.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

fn pearson_correlation(xs: &[f64], ys: &[f64]) -> Option<f64> {
    if xs.len() != ys.len() || xs.len() < 2 {
        return None;
    }
    let mx = mean(xs);
    let my = mean(ys);
    let sdx = stddev(xs);
    let sdy = stddev(ys);
    if sdx < 1e-9 || sdy < 1e-9 {
        return None;
    }
    let cov: f64 = xs
        .iter()
        .zip(ys.iter())
        .map(|(x, y)| (x - mx) * (y - my))
        .sum::<f64>()
        / (xs.len() as f64 - 1.0);
    Some(cov / (sdx * sdy))
}

/// 标准正态分布 CDF 近似（Abramowitz & Stegun 7.1.26）
fn normal_cdf(z: f64, _mu: f64, _sigma: f64) -> f64 {
    0.5 * (1.0 + erf_approx(z / std::f64::consts::SQRT_2))
}

/// erf 近似（最大误差 ~1.5e-7）
fn erf_approx(x: f64) -> f64 {
    // Abramowitz & Stegun 7.1.26
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + p * x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x).exp();
    sign * y
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WalkForwardConfig;
    use crate::metrics::{FoldResult, ISMetrics, OOSMetrics};
    use crate::split::FoldSplit;

    fn make_fold(id: usize, is_ret: f64, oos_ret: f64) -> FoldResult {
        FoldResult::new(
            id,
            FoldSplit {
                fold_id: id,
                train_start: 0,
                train_end: 100,
                validation_start: 100,
                validation_end: 100,
                test_start: 100,
                test_end: 150,
            },
            ISMetrics {
                total_return: is_ret,
                ..ISMetrics::default()
            },
            OOSMetrics {
                total_return: oos_ret,
                ..OOSMetrics::default()
            },
        )
    }

    #[test]
    fn test_aggregate_empty() {
        let (agg, stab) = aggregate_folds(&[]);
        assert_eq!(agg.mean_oos_return, 0.0);
        assert_eq!(stab.sharpe_of_sharpe, 0.0);
    }

    #[test]
    fn test_aggregate_basic() {
        let folds = vec![
            make_fold(0, 0.20, 0.10),
            make_fold(1, 0.15, 0.05),
            make_fold(2, 0.25, 0.15),
            make_fold(3, 0.30, -0.05),
        ];
        let (agg, _stab) = aggregate_folds(&folds);
        assert!((agg.mean_oos_return - 0.0625).abs() < 1e-9);
        assert_eq!(agg.pct_profitable_folds, 0.75); // 3 / 4
        assert!((agg.worst_fold_return - (-0.05)).abs() < 1e-9);
        assert!((agg.best_fold_return - 0.15).abs() < 1e-9);
    }

    #[test]
    fn test_aggregate_median() {
        let folds = vec![
            make_fold(0, 0.0, 0.10),
            make_fold(1, 0.0, 0.20),
            make_fold(2, 0.0, 0.30),
        ];
        let (agg, _) = aggregate_folds(&folds);
        assert!((agg.median_oos_return - 0.20).abs() < 1e-9);
    }

    #[test]
    fn test_deflated_sharpe_zero_std() {
        let ds = compute_deflated_sharpe(1.0, 10, 0.0);
        assert_eq!(ds, 0.0);
    }

    #[test]
    fn test_deflated_sharpe_basic() {
        // 单个 trial，observed_sharpe 远高于期望最大 → 高 Deflated Sharpe
        let ds = compute_deflated_sharpe(3.0, 1, 1.0);
        assert!(ds > 0.5, "deflated sharpe should be high: {ds}");
    }

    #[test]
    fn test_deflated_sharpe_many_trials() {
        // 100 个 trial，observed_sharpe 仅略高于平均 → 低 Deflated Sharpe
        let ds = compute_deflated_sharpe(1.5, 100, 0.1);
        // 期望最大 Sharpe ≈ sqrt(2 * ln(100)) ≈ 3.03，远高于 observed
        assert!(ds < 0.5);
    }

    #[test]
    fn test_aggregate_stability() {
        let folds = vec![
            make_fold(0, 0.0, 0.05),
            make_fold(1, 0.0, 0.10),
            make_fold(2, 0.0, 0.15),
            make_fold(3, 0.0, 0.20),
        ];
        let (_agg, stab) = aggregate_folds(&folds);
        // 收益单调递增 → 自相关接近 1
        assert!(stab.return_autocorrelation > 0.5);
    }

    #[test]
    fn test_mean_helper() {
        assert!((mean(&[]) - 0.0).abs() < 1e-9);
        assert!((mean(&[1.0, 2.0, 3.0]) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn test_stddev_helper() {
        assert_eq!(stddev(&[1.0]), 0.0);
        assert!((stddev(&[1.0, 2.0, 3.0]) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_normal_cdf_symmetry() {
        let p_neg = normal_cdf(-1.0, 0.0, 1.0);
        let p_pos = normal_cdf(1.0, 0.0, 1.0);
        assert!(((1.0 - p_neg) - p_pos).abs() < 1e-5);
    }

    // 额外测试：避免 WalkForwardConfig 导入未被使用
    #[test]
    fn test_config_import_smoke() {
        let _cfg = WalkForwardConfig::expanding(100, 50, 50);
    }
}
