//! 场景 3：HPO 超参数优化全流程
//!
//! 验证：Pareto 前沿 → 超体积计算 → 多目标优化

use std::collections::HashMap;

use axon_hpo::config::StudyDirection;
use axon_hpo::pareto::{compute_hypervolume, compute_pareto_front};
use axon_hpo::trial::{TrialResult, TrialState};

fn make_trial(trial_id: i32, values: Vec<f64>) -> TrialResult {
    TrialResult::new(trial_id, HashMap::new(), values).with_state(TrialState::Complete)
}

/// 场景 3.3: 运行优化（mock trial 结果）
pub fn run_hpo_with_mock_trials() {
    let trials: Vec<TrialResult> = (0..20)
        .map(|i| {
            let x = i as f64 / 20.0;
            let value = -((x - 0.5).powi(2)); // 抛物面，最大值在 x=0.5
            make_trial(i, vec![value])
        })
        .collect();
    assert_eq!(trials.len(), 20);
    let best = trials
        .iter()
        .filter(|t| t.state.is_complete())
        .max_by(|a, b| a.values[0].partial_cmp(&b.values[0]).unwrap())
        .unwrap();
    assert!(
        best.values[0] > -0.1,
        "最佳值应接近 0，实际 {}",
        best.values[0]
    );
}

/// 场景 3.4: 验证 Pareto 前沿（单目标）
pub fn run_pareto_front_single_objective() {
    let trials = vec![
        make_trial(0, vec![1.0]),
        make_trial(1, vec![2.0]),
        make_trial(2, vec![0.5]),
        make_trial(3, vec![3.0]),
        make_trial(4, vec![1.5]),
    ];
    let directions = vec![StudyDirection::Maximize];
    let front = compute_pareto_front(&trials, &directions).unwrap();
    assert_eq!(
        front.points.len(),
        1,
        "单目标最大化应只有 1 个 Pareto 最优点"
    );
    assert_eq!(front.points[0].objectives[0], 3.0);
}

/// 场景 3.5: 验证超体积计算
pub fn run_hypervolume_verification() {
    // 使用 minimize 方向，reference 为最差点
    let trials = vec![make_trial(0, vec![1.0, 2.0]), make_trial(1, vec![2.0, 1.0])];
    let directions = vec![StudyDirection::Minimize, StudyDirection::Minimize];
    let reference = vec![5.0, 5.0]; // nadir 参考点
    let hv = compute_hypervolume(&trials, &directions, &reference).unwrap();
    assert!(hv > 0.0, "超体积应 > 0，实际 {}", hv);
}

/// 多目标 Pareto 前沿验证
pub fn run_multi_objective_pareto() {
    let trials = vec![
        make_trial(0, vec![1.0, 3.0]),
        make_trial(1, vec![2.0, 2.0]),
        make_trial(2, vec![3.0, 1.0]),
        make_trial(3, vec![1.5, 2.5]), // 非支配（不被任何其他点支配）
        make_trial(4, vec![0.5, 0.5]), // 被 (1,3) 和 (3,1) 等支配
    ];
    let directions = vec![StudyDirection::Maximize, StudyDirection::Maximize];
    let front = compute_pareto_front(&trials, &directions).unwrap();
    // (1,3), (2,2), (3,1), (1.5,2.5) 互不支配；(0.5,0.5) 被支配
    assert_eq!(front.points.len(), 4, "应有 4 个非支配点");
}

/// 空 trial 列表不应报错
pub fn run_empty_trials() {
    let trials: Vec<TrialResult> = vec![];
    let directions = vec![StudyDirection::Maximize];
    let front = compute_pareto_front(&trials, &directions).unwrap();
    assert!(front.points.is_empty());
}
