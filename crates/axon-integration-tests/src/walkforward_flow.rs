//! 场景 4：Walk-Forward 验证全流程
//!
//! 验证：配置 → 分割 → 泄漏检测 → embargo → deflated Sharpe

use axon_walk_forward::config::{WalkForwardConfig, WindowType};
use axon_walk_forward::evaluation::compute_deflated_sharpe;
use axon_walk_forward::purge::{detect_leakage, embargo_indices, purge_overlapping_labels};
use axon_walk_forward::split::TimeSeriesSplitter;

/// 场景 4.1: 创建配置
pub fn run_walkforward_config_creation() {
    let cfg = WalkForwardConfig::rolling(200, 100, 50);
    assert_eq!(cfg.train_size, 200);
    assert_eq!(cfg.test_size, 100);
    assert_eq!(cfg.step_size, 50);
    assert_eq!(cfg.window_type, WindowType::Rolling);
}

/// 场景 4.2: 配置 splits + purge + embargo
pub fn run_splits_purge_embargo_config() {
    let mut cfg = WalkForwardConfig::rolling(200, 100, 50);
    cfg.purge_gap = 10;
    cfg.embargo_pct = 0.05;
    assert_eq!(cfg.purge_gap, 10);
    assert!((cfg.embargo_pct - 0.05).abs() < f64::EPSILON);
}

/// 场景 4.3: 运行前向验证（生成 fold 结果）
pub fn run_forward_validation_folds() {
    let cfg = WalkForwardConfig::rolling(200, 100, 50);
    let splitter = TimeSeriesSplitter::new(cfg);
    let folds = splitter.split(1000);
    assert!(!folds.is_empty(), "应生成至少 1 个 fold");
    for f in &folds {
        assert!(f.train_end > f.train_start, "train 区间应正");
        assert!(f.test_end > f.test_start, "test 区间应正");
        assert!(f.test_start >= f.train_end, "test 应在 train 之后");
    }
}

/// 场景 4.4: 验证泄漏检测
pub fn run_leakage_detection() {
    // 有重叠 → 应检测到
    let train: Vec<usize> = (0..100).collect();
    let test: Vec<usize> = (95..150).collect();
    let (has, pairs) = detect_leakage(&train, &test, 0);
    assert!(has, "train/test 重叠应检测到泄漏");
    assert!(!pairs.is_empty());
}

/// 场景 4.5: 验证 embargo 正确排除重叠索引
pub fn run_embargo_exclusion() {
    let test: Vec<usize> = (100..150).collect();
    let embargoed = embargo_indices(&test, 0.1, 200);
    assert!(!embargoed.is_empty(), "embargo 应产生排除索引");
    // embargo 起点 = test_max + 1 = 150
    assert_eq!(embargoed[0], 150);
}

/// purge 验证
pub fn run_purge_overlapping_labels() {
    let train: Vec<usize> = (0..100).collect();
    let test: Vec<usize> = (100..150).collect();
    let purged = purge_overlapping_labels(&train, &test, 10);
    // 应移除 train 中 >= 90 的样本（test_start=100, horizon=10, cutoff=90）
    assert_eq!(purged.len(), 90);
    assert!(purged.iter().all(|&i| i < 90));
}

/// 场景 4.6: 验证 deflated Sharpe（dsr < 原始 sharpe）
pub fn run_deflated_sharpe() {
    let observed = 2.0f64;
    let dsr = compute_deflated_sharpe(observed, 50, 0.5);
    assert!(dsr < observed, "deflated Sharpe 应 < 原始: {} vs {}", dsr, observed);
    // 更多 trial → 更大惩罚
    let dsr_more = compute_deflated_sharpe(observed, 200, 0.5);
    assert!(dsr_more < dsr, "更多 trial 应产生更低的 deflated Sharpe");
}

/// Rolling vs Expanding 窗口差异验证
pub fn run_window_type_difference() {
    let rolling_cfg = WalkForwardConfig::rolling(200, 100, 100);
    let expanding_cfg = WalkForwardConfig::expanding(200, 100, 100);
    let rolling_folds = TimeSeriesSplitter::new(rolling_cfg).split(1000);
    let expanding_folds = TimeSeriesSplitter::new(expanding_cfg).split(1000);
    assert!(!rolling_folds.is_empty());
    assert!(!expanding_folds.is_empty());
    // Rolling: 每个 fold 的 train_size 固定
    for f in &rolling_folds {
        assert_eq!(f.train_size(), 200);
    }
    // Expanding: train 从 0 开始
    for f in &expanding_folds {
        assert_eq!(f.train_start, 0);
    }
}
