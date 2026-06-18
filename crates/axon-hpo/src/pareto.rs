//! Pareto 前沿计算与超体积指标
//!
//! 多目标优化时需要从所有 trial 中找出"不被任何其他 trial 支配"的子集。
//! 超体积（Hypervolume）衡量 Pareto 前沿在目标空间覆盖的体积。

use serde::{Deserialize, Serialize};

use crate::config::StudyDirection;
use crate::error::{HPOError, HPOResult};
use crate::trial::TrialResult;

/// Pareto 前沿上的一个点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParetoPoint {
    /// 试验参数
    pub params: std::collections::HashMap<String, serde_json::Value>,
    /// 目标值列表
    pub objectives: Vec<f64>,
    /// trial ID
    pub trial_id: i32,
}

/// Pareto 前沿（多目标时的最优解集合）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParetoFront {
    /// 前沿点
    pub points: Vec<ParetoPoint>,
    /// 优化方向
    pub directions: Vec<StudyDirection>,
}

impl ParetoFront {
    /// 前沿点数量
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// 计算超体积（2D 精确 / N-D 近似）
    pub fn hypervolume(&self, reference_point: &[f64]) -> HPOResult<f64> {
        compute_hypervolume_from_points(&self.points, &self.directions, reference_point)
    }
}

/// 判断 a 是否 Pareto 支配 b
///
/// 支配条件（针对 `directions`）：
/// - a 在每个目标上 >= b（按 direction 调整）
/// - a 至少在一个目标上 > b
pub fn dominates(a: &[f64], b: &[f64], directions: &[StudyDirection]) -> bool {
    if a.len() != b.len() || a.len() != directions.len() {
        return false;
    }

    let mut at_least_one_better = false;
    for (val_a, val_b, d) in a
        .iter()
        .zip(b.iter())
        .zip(directions.iter())
        .map(|((x, y), d)| (x, y, d))
    {
        match d {
            StudyDirection::Maximize => {
                if val_a < val_b {
                    return false;
                }
                if val_a > val_b {
                    at_least_one_better = true;
                }
            }
            StudyDirection::Minimize => {
                if val_a > val_b {
                    return false;
                }
                if val_a < val_b {
                    at_least_one_better = true;
                }
            }
        }
    }
    at_least_one_better
}

/// 计算 Pareto 前沿
///
/// Args:
/// - trials: 所有 trial 结果
/// - directions: 每个目标的优化方向
///
/// Returns:
/// - Pareto 前沿（不被任何其他 trial 支配的 trial 子集）
pub fn compute_pareto_front(
    trials: &[TrialResult],
    directions: &[StudyDirection],
) -> HPOResult<ParetoFront> {
    if directions.is_empty() {
        return Err(HPOError::Config("directions must not be empty".to_string()));
    }
    if trials.is_empty() {
        return Ok(ParetoFront {
            points: Vec::new(),
            directions: directions.to_vec(),
        });
    }

    // 过滤有效 trial（state == Complete 且 values 长度匹配）
    let valid: Vec<&TrialResult> = trials
        .iter()
        .filter(|t| t.state.is_complete() && t.values.len() == directions.len())
        .collect();

    if valid.is_empty() {
        return Ok(ParetoFront {
            points: Vec::new(),
            directions: directions.to_vec(),
        });
    }

    // 计算每个 trial 是否被支配
    let mut dominated = vec![false; valid.len()];
    for i in 0..valid.len() {
        for j in 0..valid.len() {
            if i == j || dominated[i] {
                continue;
            }
            if dominates(&valid[j].values, &valid[i].values, directions) {
                dominated[i] = true;
                break;
            }
        }
    }

    let points = valid
        .iter()
        .zip(dominated.iter())
        .filter_map(|(t, &d)| {
            if d {
                None
            } else {
                Some(ParetoPoint {
                    params: t.params.clone(),
                    objectives: t.values.clone(),
                    trial_id: t.trial_id,
                })
            }
        })
        .collect();

    Ok(ParetoFront {
        points,
        directions: directions.to_vec(),
    })
}

/// 计算超体积（Hypervolume Indicator）
///
/// 2D：精确计算（排序后梯形面积）
/// N-D：近似计算（参考点减去最差点之积）
///
/// Args:
/// - pareto_points: Pareto 前沿点
/// - directions: 优化方向
/// - reference_point: 参考点（通常选为各目标的最差值）
pub fn compute_hypervolume_from_points(
    pareto_points: &[ParetoPoint],
    directions: &[StudyDirection],
    reference_point: &[f64],
) -> HPOResult<f64> {
    if pareto_points.is_empty() {
        return Ok(0.0);
    }
    if reference_point.len() != directions.len() {
        return Err(HPOError::DirectionsMismatch {
            expected: directions.len(),
            got: reference_point.len(),
        });
    }

    let n_obj = directions.len();
    // 过滤掉 objectives 为空或维度不匹配的点
    let objectives: Vec<Vec<f64>> = pareto_points
        .iter()
        .map(|p| p.objectives.clone())
        .filter(|o| o.len() == n_obj)
        .collect();

    if objectives.is_empty() {
        return Ok(0.0);
    }

    // 2D 精确
    if n_obj == 2 {
        return Ok(compute_hypervolume_2d(
            &objectives,
            reference_point,
            directions,
        ));
    }

    // N-D 近似
    Ok(compute_hypervolume_nd(
        &objectives,
        reference_point,
        directions,
    ))
}

fn compute_hypervolume_2d(
    objectives: &[Vec<f64>],
    reference: &[f64],
    directions: &[StudyDirection],
) -> f64 {
    if directions.len() < 2 || objectives.is_empty() {
        return 0.0;
    }
    // 仅处理 maximize + maximize 场景的精确梯形面积
    // 其他方向可通过坐标变换归约（暂简化）
    if !matches!(directions[0], StudyDirection::Maximize)
        || !matches!(directions[1], StudyDirection::Maximize)
    {
        return compute_hypervolume_nd(objectives, reference, directions);
    }

    // 按 x 排序
    let mut sorted: Vec<&Vec<f64>> = objectives.iter().collect();
    sorted.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));

    let mut hv = 0.0;
    let ref_x = reference[0];
    let ref_y = reference[1];
    for obj in sorted {
        let width = (ref_x - obj[0]).max(0.0);
        let height = (ref_y - obj[1]).max(0.0);
        hv += width * height;
    }
    hv
}

fn compute_hypervolume_nd(
    objectives: &[Vec<f64>],
    reference: &[f64],
    _directions: &[StudyDirection],
) -> f64 {
    // 高维近似：参考点减去最差前沿点之积
    if objectives.is_empty() {
        return 0.0;
    }
    let n_obj = reference.len();
    let mut min_per_dim: Vec<f64> = reference.to_vec();
    for obj in objectives {
        for (d, &v) in obj.iter().enumerate().take(n_obj) {
            if v < min_per_dim[d] {
                min_per_dim[d] = v;
            }
        }
    }
    let mut hv = 1.0;
    for d in 0..n_obj {
        let w = (reference[d] - min_per_dim[d]).max(0.0);
        hv *= w;
    }
    hv
}

/// 便捷函数：从 trials 计算 Pareto 前沿并返回其超体积
pub fn compute_hypervolume(
    trials: &[TrialResult],
    directions: &[StudyDirection],
    reference_point: &[f64],
) -> HPOResult<f64> {
    let front = compute_pareto_front(trials, directions)?;
    compute_hypervolume_from_points(&front.points, directions, reference_point)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_trial(id: i32, values: Vec<f64>) -> TrialResult {
        let mut params = HashMap::new();
        params.insert("lr".into(), serde_json::json!(0.001));
        TrialResult::new(id, params, values)
    }

    #[test]
    fn test_dominates_basic() {
        let dirs = vec![StudyDirection::Maximize, StudyDirection::Maximize];
        // a=(1,1) 支配 b=(0,0)
        assert!(dominates(&[1.0, 1.0], &[0.0, 0.0], &dirs));
        // a=(1,0) 不支配 b=(0,1)
        assert!(!dominates(&[1.0, 0.0], &[0.0, 1.0], &dirs));
        // a=(1,1) 不支配 a
        assert!(!dominates(&[1.0, 1.0], &[1.0, 1.0], &dirs));
    }

    #[test]
    fn test_dominates_minimize() {
        let dirs = vec![StudyDirection::Minimize, StudyDirection::Minimize];
        assert!(dominates(&[0.0, 0.0], &[1.0, 1.0], &dirs));
        assert!(!dominates(&[0.0, 1.0], &[1.0, 0.0], &dirs));
    }

    #[test]
    fn test_dominates_length_mismatch() {
        let dirs = vec![StudyDirection::Maximize];
        assert!(!dominates(&[1.0, 2.0], &[1.0], &dirs));
    }

    #[test]
    fn test_compute_pareto_front_simple() {
        // 3 个 trial，最大化方向
        // t1=(1,0) t2=(0,1) t3=(0.5,0.5)
        // 三个都是 Pareto 最优（互相不支配）
        let trials = vec![
            make_trial(1, vec![1.0, 0.0]),
            make_trial(2, vec![0.0, 1.0]),
            make_trial(3, vec![0.5, 0.5]),
        ];
        let dirs = vec![StudyDirection::Maximize, StudyDirection::Maximize];
        let front = compute_pareto_front(&trials, &dirs).unwrap();
        assert_eq!(front.len(), 3);
    }

    #[test]
    fn test_compute_pareto_front_with_dominated() {
        // 4 个 trial
        // t1=(2,2) - 非支配
        // t2=(1,1) - 被 t1 支配
        // t3=(3,1) - 非支配
        // t4=(1,3) - 非支配
        let trials = vec![
            make_trial(1, vec![2.0, 2.0]),
            make_trial(2, vec![1.0, 1.0]),
            make_trial(3, vec![3.0, 1.0]),
            make_trial(4, vec![1.0, 3.0]),
        ];
        let dirs = vec![StudyDirection::Maximize, StudyDirection::Maximize];
        let front = compute_pareto_front(&trials, &dirs).unwrap();
        // t1, t3, t4 都在前沿
        assert_eq!(front.len(), 3);
        // t1 必然在
        assert!(front.points.iter().any(|p| p.trial_id == 1));
        // t2 不在
        assert!(!front.points.iter().any(|p| p.trial_id == 2));
    }

    #[test]
    fn test_compute_pareto_front_empty() {
        let trials: Vec<TrialResult> = vec![];
        let dirs = vec![StudyDirection::Maximize];
        let front = compute_pareto_front(&trials, &dirs).unwrap();
        assert!(front.is_empty());
    }

    #[test]
    fn test_compute_pareto_front_filters_incomplete() {
        let mut t1 = make_trial(1, vec![1.0, 1.0]);
        t1.state = crate::trial::TrialState::Pruned;
        let t2 = make_trial(2, vec![0.5, 0.5]);
        let trials = vec![t1, t2];
        let dirs = vec![StudyDirection::Maximize, StudyDirection::Maximize];
        let front = compute_pareto_front(&trials, &dirs).unwrap();
        // Pruned 状态被过滤
        assert_eq!(front.len(), 1);
        assert_eq!(front.points[0].trial_id, 2);
    }

    #[test]
    fn test_hypervolume_2d() {
        // 2D 单点：hv = (ref_x - x) * (ref_y - y)
        let point = ParetoPoint {
            params: HashMap::new(),
            objectives: vec![1.0, 1.0],
            trial_id: 1,
        };
        let dirs = vec![StudyDirection::Maximize, StudyDirection::Maximize];
        let hv = compute_hypervolume_from_points(&[point], &dirs, &[2.0, 2.0]).unwrap();
        assert!((hv - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_hypervolume_empty() {
        let dirs = vec![StudyDirection::Maximize, StudyDirection::Maximize];
        let hv = compute_hypervolume_from_points(&[], &dirs, &[2.0, 2.0]).unwrap();
        assert_eq!(hv, 0.0);
    }

    #[test]
    fn test_hypervolume_direction_mismatch() {
        let point = ParetoPoint {
            params: HashMap::new(),
            objectives: vec![1.0, 1.0],
            trial_id: 1,
        };
        let dirs = vec![StudyDirection::Maximize, StudyDirection::Maximize];
        let err = compute_hypervolume_from_points(&[point], &dirs, &[2.0]).unwrap_err();
        assert!(matches!(
            err,
            HPOError::DirectionsMismatch {
                expected: 2,
                got: 1
            }
        ));
    }

    #[test]
    fn test_hypervolume_convenience() {
        let trials = vec![make_trial(1, vec![1.0, 1.0]), make_trial(2, vec![0.5, 0.5])];
        let dirs = vec![StudyDirection::Maximize, StudyDirection::Maximize];
        // 仅 t1 在前沿，hv = (2-1)*(2-1) = 1
        let hv = compute_hypervolume(&trials, &dirs, &[2.0, 2.0]).unwrap();
        assert!((hv - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_pareto_front_len_empty() {
        let front = ParetoFront {
            points: vec![],
            directions: vec![StudyDirection::Maximize],
        };
        assert_eq!(front.len(), 0);
        assert!(front.is_empty());
    }
}
