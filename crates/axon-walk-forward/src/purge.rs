//! Purge / Embargo / Leakage 检测
//!
//! 防泄漏是时间序列交叉验证的核心约束：
//! - **Purge**：移除训练集中与测试集标签重叠的样本
//! - **Embargo**：测试集后添加隔离期，防止自相关泄漏
//! - **Leakage 检测**：校验 train/test 严格分离

use crate::metrics::LeakageCheck;

/// Purge 训练集：移除索引 >= (test_start - label_horizon) 的样本
///
/// Args:
/// - train_idx: 训练集索引
/// - test_idx: 测试集索引
/// - label_horizon: 标签前瞻步数
///
/// Returns:
/// - 清洗后的训练集索引
pub fn purge_overlapping_labels(
    train_idx: &[usize],
    test_idx: &[usize],
    label_horizon: usize,
) -> Vec<usize> {
    if test_idx.is_empty() || label_horizon == 0 {
        return train_idx.to_vec();
    }
    let test_start = *test_idx.iter().min().expect("non-empty test_idx");
    let cutoff = test_start.saturating_sub(label_horizon);
    train_idx.iter().copied().filter(|&i| i < cutoff).collect()
}

/// Embargo 索引：在测试集之后添加隔离期
///
/// Args:
/// - test_idx: 测试集索引
/// - embargo_pct: embargo 占测试集比例（0.0~1.0）
/// - n_total: 总样本数
///
/// Returns:
/// - 需要 embargo 的索引范围
pub fn embargo_indices(test_idx: &[usize], embargo_pct: f64, n_total: usize) -> Vec<usize> {
    if test_idx.is_empty() || embargo_pct <= 0.0 {
        return Vec::new();
    }
    let test_end = *test_idx.iter().max().expect("non-empty test_idx");
    let embargo_size = ((test_idx.len() as f64) * embargo_pct).ceil() as usize;
    let embargo_size = embargo_size.max(1);
    let start = test_end + 1;
    let end = (start + embargo_size).min(n_total);
    if start >= n_total {
        return Vec::new();
    }
    (start..end).collect()
}

/// 检测训练集与测试集之间是否存在数据泄漏
///
/// Returns:
/// - `(has_leakage, leaked_pairs)`：leaked_pairs 是 (train_idx, test_idx) 元组列表
pub fn detect_leakage(
    train_idx: &[usize],
    test_idx: &[usize],
    feature_lag: usize,
) -> (bool, Vec<(usize, usize)>) {
    if train_idx.is_empty() || test_idx.is_empty() {
        return (false, Vec::new());
    }

    // 1. 直接索引重叠
    let train_set: std::collections::HashSet<usize> = train_idx.iter().copied().collect();
    let test_set: std::collections::HashSet<usize> = test_idx.iter().copied().collect();
    let overlap: Vec<usize> = train_set.intersection(&test_set).copied().collect();
    if !overlap.is_empty() {
        let pairs: Vec<(usize, usize)> = overlap.iter().map(|&i| (i, i)).collect();
        return (true, pairs);
    }

    // 2. 时间邻近性泄漏（test_min - train_max <= feature_lag）
    if feature_lag > 0 {
        let train_max = *train_idx.iter().max().expect("non-empty");
        let test_min = *test_idx.iter().min().expect("non-empty");
        if test_min.saturating_sub(train_max) <= feature_lag {
            return (true, vec![(train_max, test_min)]);
        }
    }

    (false, Vec::new())
}

/// 便捷函数：返回结构化的泄漏检测报告
pub fn leakage_check(train_idx: &[usize], test_idx: &[usize], feature_lag: usize) -> LeakageCheck {
    let (has_leakage, leaked_indices) = detect_leakage(train_idx, test_idx, feature_lag);
    let details = if has_leakage {
        format!(
            "leakage detected: {} leaked pairs, feature_lag={}",
            leaked_indices.len(),
            feature_lag
        )
    } else {
        "no leakage".to_string()
    };
    LeakageCheck {
        has_leakage,
        leaked_indices,
        details,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_purge_basic() {
        // train: 0..100, test: 100..150, label_horizon: 5
        // 应移除 train 中索引 >= 95 的样本
        let train: Vec<usize> = (0..100).collect();
        let test: Vec<usize> = (100..150).collect();
        let purged = purge_overlapping_labels(&train, &test, 5);
        assert_eq!(purged.len(), 95);
        assert!(purged.iter().all(|&i| i < 95));
    }

    #[test]
    fn test_purge_zero_horizon() {
        let train = vec![0, 1, 2, 3];
        let test = vec![5, 6];
        let purged = purge_overlapping_labels(&train, &test, 0);
        assert_eq!(purged, train);
    }

    #[test]
    fn test_purge_empty_test() {
        let train = vec![0, 1, 2];
        let purged = purge_overlapping_labels(&train, &[], 5);
        assert_eq!(purged, train);
    }

    #[test]
    fn test_embargo_basic() {
        // test: 100..150 (50 个，最大索引 149), embargo_pct: 0.1 → 5 个索引
        // start = 149 + 1 = 150, end = 150 + 5 = 155
        let test: Vec<usize> = (100..150).collect();
        let embargoed = embargo_indices(&test, 0.1, 200);
        assert_eq!(embargoed, vec![150, 151, 152, 153, 154]);
    }

    #[test]
    fn test_embargo_zero_pct() {
        let test = vec![10, 20];
        let embargoed = embargo_indices(&test, 0.0, 100);
        assert!(embargoed.is_empty());
    }

    #[test]
    fn test_embargo_clamp_to_total() {
        // test: 195..200, total=200, embargo_pct=1.0 → 5 个但越界
        let test: Vec<usize> = (195..200).collect();
        let embargoed = embargo_indices(&test, 1.0, 200);
        assert!(embargoed.is_empty()); // 越界
    }

    #[test]
    fn test_detect_leakage_overlap() {
        let train = vec![1, 2, 3, 4];
        let test = vec![3, 4, 5, 6];
        let (has, pairs) = detect_leakage(&train, &test, 0);
        assert!(has);
        assert_eq!(pairs.len(), 2); // 3 和 4
    }

    #[test]
    fn test_detect_leakage_no_overlap() {
        let train = vec![0, 1, 2, 3];
        let test = vec![10, 11, 12];
        let (has, pairs) = detect_leakage(&train, &test, 0);
        assert!(!has);
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_detect_leakage_lag() {
        // train: 0..100, test: 102..110, feature_lag=5 → 102-100=2 <= 5 → 泄漏
        let train: Vec<usize> = (0..100).collect();
        let test: Vec<usize> = (102..110).collect();
        let (has, pairs) = detect_leakage(&train, &test, 5);
        assert!(has);
        assert_eq!(pairs, vec![(99, 102)]);
    }

    #[test]
    fn test_leakage_check_struct() {
        let train = vec![1, 2, 3];
        let test = vec![2, 3, 4];
        let report = leakage_check(&train, &test, 0);
        assert!(report.has_leakage);
        assert!(!report.details.is_empty());
    }
}
