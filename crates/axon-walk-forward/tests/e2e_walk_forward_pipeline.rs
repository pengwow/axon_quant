//! 端到端测试:axon-walk-forward 滚动前向验证完整流程
//!
//! ## 5 个测试场景
//!
//! 1. `wf_rolling_window_split_pipeline`:Rolling 窗口 → split → 验证 fold 数量和区间
//! 2. `wf_expanding_window_split_pipeline`:Expanding 窗口 → split → 验证 train 从 0 开始
//! 3. `wf_leakage_detection_pipeline`:构造有/无泄漏的 split → detect_leakage → 验证
//! 4. `wf_purge_embargo_pipeline`:train/test → purge → embargo → 验证索引清洗
//! 5. `wf_aggregate_and_deflated_sharpe`:构造 FoldResult → aggregate → deflated_sharpe → 验证
//!
//! 运行:`cargo test -p axon-walk-forward --test e2e_walk_forward_pipeline`

use axon_walk_forward::{
    FoldResult, ISMetrics, OOSMetrics, TimeSeriesSplitter, WalkForwardConfig, aggregate_folds,
    compute_deflated_sharpe, detect_leakage, embargo_indices, purge_overlapping_labels,
};

// ── helpers ────────────────────────────────────────────────────────────

fn make_fold_result(
    fold_id: usize,
    is_return: f64,
    oos_return: f64,
    oos_sharpe: f64,
) -> FoldResult {
    let split = axon_walk_forward::FoldSplit {
        fold_id,
        train_start: fold_id * 100,
        train_end: fold_id * 100 + 200,
        validation_start: fold_id * 100 + 200,
        validation_end: fold_id * 100 + 220,
        test_start: fold_id * 100 + 225,
        test_end: fold_id * 100 + 325,
    };
    let is_metrics = ISMetrics {
        total_return: is_return,
        sharpe_ratio: is_return * 2.0,
        max_drawdown: -is_return * 0.1,
        win_rate: 0.55,
        profit_factor: 1.2,
    };
    let oos_metrics = OOSMetrics {
        total_return: oos_return,
        sharpe_ratio: oos_sharpe,
        max_drawdown: -oos_return * 0.1,
        win_rate: 0.52,
        profit_factor: 1.1,
        calmar_ratio: oos_return * 5.0,
    };
    FoldResult::new(fold_id, split, is_metrics, oos_metrics)
}

// ── 1. Rolling 窗口 split pipeline ────────────────────────────────────

#[test]
fn wf_rolling_window_split_pipeline() {
    let cfg = WalkForwardConfig::rolling(200, 100, 100);
    let splitter = TimeSeriesSplitter::new(cfg);
    let folds = splitter.split(1000);

    // 验证 fold 数量
    assert_eq!(folds.len(), 8);

    // 验证每个 fold 的 train 窗口固定 200
    for f in &folds {
        assert_eq!(f.train_size(), 200);
    }

    // 验证 test 区间不重叠
    for w in folds.windows(2) {
        assert!(w[1].test_start >= w[0].test_end);
    }
}

// ── 2. Expanding 窗口 split pipeline ──────────────────────────────────

#[test]
fn wf_expanding_window_split_pipeline() {
    let cfg = WalkForwardConfig::expanding(200, 100, 100);
    let splitter = TimeSeriesSplitter::new(cfg);
    let folds = splitter.split(1000);

    // 验证 fold 数量
    assert_eq!(folds.len(), 8);

    // 验证每个 fold 的 train 从 0 开始
    for f in &folds {
        assert_eq!(f.train_start, 0);
    }

    // 验证 train 窗口递增
    for w in folds.windows(2) {
        assert!(w[1].train_end > w[0].train_end);
    }
}

// ── 3. Leakage detection pipeline ─────────────────────────────────────

#[test]
fn wf_leakage_detection_pipeline() {
    // 无泄漏:train 和 test 完全分离
    let train_clean: Vec<usize> = (0..100).collect();
    let test_clean: Vec<usize> = (150..200).collect();
    let (has_leak_clean, _) = detect_leakage(&train_clean, &test_clean, 0);
    assert!(!has_leak_clean);

    // 有泄漏:索引重叠
    let train_leak: Vec<usize> = (0..100).collect();
    let test_leak: Vec<usize> = (95..150).collect();
    let (has_leak, pairs_leak) = detect_leakage(&train_leak, &test_leak, 0);
    assert!(has_leak);
    assert!(!pairs_leak.is_empty());

    // 有泄漏:时间邻近性
    let train_near: Vec<usize> = (0..100).collect();
    let test_near: Vec<usize> = (102..150).collect();
    let (_, pairs) = detect_leakage(&train_near, &test_near, 5);
    assert!(!pairs.is_empty());
}

// ── 4. Purge + embargo pipeline ────────────────────────────────────────

#[test]
fn wf_purge_embargo_pipeline() {
    let train: Vec<usize> = (0..200).collect();
    let test: Vec<usize> = (200..250).collect();

    // Purge: 移除 train 中与 test 标签重叠的样本
    let purged = purge_overlapping_labels(&train, &test, 10);
    assert!(purged.len() < train.len());
    assert!(purged.iter().all(|&i| i < 190));

    // Embargo: 在 test 之后添加隔离期
    let embargoed = embargo_indices(&test, 0.1, 500);
    assert!(!embargoed.is_empty());
    assert!(embargoed[0] >= 250);
}

// ── 5. Aggregate + deflated sharpe ────────────────────────────────────

#[test]
fn wf_aggregate_and_deflated_sharpe() {
    // 构造 5 个 fold 结果
    let folds = vec![
        make_fold_result(0, 0.05, 0.03, 1.2),
        make_fold_result(1, 0.08, 0.06, 1.5),
        make_fold_result(2, 0.03, 0.02, 0.8),
        make_fold_result(3, 0.06, 0.04, 1.1),
        make_fold_result(4, 0.04, 0.03, 0.9),
    ];

    // 聚合
    let (aggregated, stability) = aggregate_folds(&folds);

    // 验证聚合指标
    assert!(aggregated.mean_oos_return > 0.0);
    assert!(aggregated.mean_oos_sharpe > 0.0);
    assert!(aggregated.pct_profitable_folds > 0.0);
    assert!(aggregated.pct_profitable_folds <= 1.0);

    // 验证稳定性指标
    assert!(stability.deflated_sharpe >= 0.0);
    assert!(stability.probability_of_loss >= 0.0);
    assert!(stability.probability_of_loss <= 1.0);

    // 单独测试 deflated sharpe
    let ds = compute_deflated_sharpe(1.5, 100, 0.5);
    assert!(ds > 0.0);
    assert!(ds <= 1.0);
}
