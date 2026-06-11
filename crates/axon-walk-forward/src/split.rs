//! 时间序列分割：FoldSplit + TimeSeriesSplitter

use serde::{Deserialize, Serialize};

use crate::config::WalkForwardConfig;
use crate::config::WindowType;

/// 单个 fold 的索引分割信息
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldSplit {
    /// fold 序号
    pub fold_id: usize,
    /// 训练集起始索引（包含）
    pub train_start: usize,
    /// 训练集结束索引（不包含）
    pub train_end: usize,
    /// 验证集起始索引（包含）
    pub validation_start: usize,
    /// 验证集结束索引（不包含）
    pub validation_end: usize,
    /// 测试集起始索引（包含）
    pub test_start: usize,
    /// 测试集结束索引（不包含）
    pub test_end: usize,
}

impl FoldSplit {
    /// 训练集大小
    pub fn train_size(&self) -> usize {
        self.train_end - self.train_start
    }

    /// 验证集大小
    pub fn val_size(&self) -> usize {
        self.validation_end - self.validation_start
    }

    /// 测试集大小
    pub fn test_size(&self) -> usize {
        self.test_end - self.test_start
    }

    /// 训练集索引范围（`train_start..train_end`）
    pub fn train_range(&self) -> std::ops::Range<usize> {
        self.train_start..self.train_end
    }

    /// 验证集索引范围（`validation_start..validation_end`）
    pub fn val_range(&self) -> std::ops::Range<usize> {
        self.validation_start..self.validation_end
    }

    /// 测试集索引范围（`test_start..test_end`）
    pub fn test_range(&self) -> std::ops::Range<usize> {
        self.test_start..self.test_end
    }
}

/// 时间序列分割器
pub struct TimeSeriesSplitter {
    config: WalkForwardConfig,
}

impl TimeSeriesSplitter {
    /// 构造分割器
    pub fn new(config: WalkForwardConfig) -> Self {
        Self { config }
    }

    /// 获取配置引用
    pub fn config(&self) -> &WalkForwardConfig {
        &self.config
    }

    /// 生成所有 fold 的索引分割
    ///
    /// 返回的 fold 数取决于 `n_samples` 和配置中的窗口大小。
    /// 当剩余数据不足以生成完整 fold 时停止。
    pub fn split(&self, n_samples: usize) -> Vec<FoldSplit> {
        let cfg = &self.config;
        let mut folds = Vec::new();
        let mut fold_id = 0;

        // 第一个 fold 的"test_end" 起始位置
        // train_size + validation_size + purge_gap + test_size
        let block = cfg.train_size + cfg.validation_size + cfg.purge_gap + cfg.test_size;

        if n_samples < block {
            return folds;
        }

        // 推进位置：每个 fold 推进 step_size
        let mut step_pos = block; // 第一个 fold 结束后推进的位置

        loop {
            // test 区间
            let test_end = step_pos;
            let test_start = test_end - cfg.test_size;
            // val_end = test_start - purge_gap
            // val_start = val_end - validation_size
            // train_end = val_start
            // train_start = (Rolling) train_end - train_size; (Expanding) 0
            let val_end_s = test_start - cfg.purge_gap;
            let val_start_s = val_end_s.saturating_sub(cfg.validation_size);
            let train_end = val_start_s;
            let train_start = match cfg.window_type {
                WindowType::Rolling => train_end.saturating_sub(cfg.train_size),
                WindowType::Expanding => 0,
            };

            // 防越界：训练起点不能 > 训练终点
            if train_start > train_end {
                break;
            }

            folds.push(FoldSplit {
                fold_id,
                train_start,
                train_end,
                validation_start: val_start_s,
                validation_end: val_end_s,
                test_start,
                test_end,
            });

            fold_id += 1;
            step_pos += cfg.step_size;

            if step_pos > n_samples {
                break;
            }
        }

        folds
    }
}

/// 便捷函数：Expanding 窗口
pub fn expand_window(
    n_samples: usize,
    train_size: usize,
    test_size: usize,
    step_size: usize,
    purge_gap: usize,
) -> Vec<FoldSplit> {
    let mut cfg = WalkForwardConfig::expanding(train_size, test_size, step_size);
    cfg.purge_gap = purge_gap;
    TimeSeriesSplitter::new(cfg).split(n_samples)
}

/// 便捷函数：Rolling 窗口
pub fn rolling_window(
    n_samples: usize,
    train_size: usize,
    test_size: usize,
    step_size: usize,
    purge_gap: usize,
) -> Vec<FoldSplit> {
    let mut cfg = WalkForwardConfig::rolling(train_size, test_size, step_size);
    cfg.purge_gap = purge_gap;
    TimeSeriesSplitter::new(cfg).split(n_samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WindowType;

    #[test]
    fn test_expanding_basic() {
        // 1000 个数据点，train=200, test=100, step=100
        // 第一个 fold: train [0, 200), test [200, 300)
        // 第二个 fold: train [0, 300), test [300, 400)
        // ... 直到 test_end > 1000
        let cfg = WalkForwardConfig::expanding(200, 100, 100);
        let folds = TimeSeriesSplitter::new(cfg).split(1000);
        assert_eq!(folds.len(), 8); // 200, 300, ..., 900 → test [200..300], ..., [900..1000]
        // 检查 train 始终从 0 开始
        for f in &folds {
            assert_eq!(f.train_start, 0);
        }
    }

    #[test]
    fn test_rolling_basic() {
        // 1000 个数据点，train=200, test=100, step=100
        // 第一个 fold: train [0, 200), test [200, 300)
        // 第二个 fold: train [100, 300), test [300, 400) ← rolling
        // 第三个 fold: train [200, 400), test [400, 500)
        let cfg = WalkForwardConfig::rolling(200, 100, 100);
        let folds = TimeSeriesSplitter::new(cfg).split(1000);
        assert_eq!(folds.len(), 8);
        // 检查 train 窗口固定 200
        for f in &folds {
            assert_eq!(f.train_size(), 200);
        }
    }

    #[test]
    fn test_no_overlap() {
        let cfg = WalkForwardConfig::expanding(100, 50, 50);
        let folds = TimeSeriesSplitter::new(cfg).split(500);
        for w in folds.windows(2) {
            let prev = &w[0];
            let curr = &w[1];
            // 当前 fold 的 test 区间不应与前一个 fold 的 test 区间重叠
            assert!(curr.test_start >= prev.test_end);
        }
    }

    #[test]
    fn test_test_always_after_train() {
        let cfg = WalkForwardConfig::rolling(200, 50, 25);
        let folds = TimeSeriesSplitter::new(cfg).split(1000);
        for f in &folds {
            assert!(f.test_start >= f.train_end);
            assert!(f.test_start >= f.validation_end);
        }
    }

    #[test]
    fn test_purge_gap() {
        let mut cfg = WalkForwardConfig::expanding(100, 50, 50);
        cfg.purge_gap = 5;
        let folds = TimeSeriesSplitter::new(cfg).split(500);
        for f in &folds {
            assert_eq!(f.test_start, f.validation_end + 5);
        }
    }

    #[test]
    fn test_validation_set() {
        let mut cfg = WalkForwardConfig::expanding(100, 50, 50);
        cfg.validation_size = 20;
        let purge_gap = cfg.purge_gap;
        let folds = TimeSeriesSplitter::new(cfg).split(500);
        for f in &folds {
            assert_eq!(f.val_size(), 20);
            assert_eq!(f.test_start, f.validation_end + purge_gap);
        }
    }

    #[test]
    fn test_too_small_data() {
        let cfg = WalkForwardConfig::expanding(200, 50, 50);
        let folds = TimeSeriesSplitter::new(cfg).split(100); // < train + test
        assert!(folds.is_empty());
    }

    #[test]
    fn test_fold_split_methods() {
        let fold = FoldSplit {
            fold_id: 0,
            train_start: 0,
            train_end: 100,
            validation_start: 100,
            validation_end: 120,
            test_start: 125,
            test_end: 150,
        };
        assert_eq!(fold.train_size(), 100);
        assert_eq!(fold.val_size(), 20);
        assert_eq!(fold.test_size(), 25);
        assert_eq!(fold.train_range(), 0..100);
        assert_eq!(fold.val_range(), 100..120);
        assert_eq!(fold.test_range(), 125..150);
    }

    #[test]
    fn test_expand_window_helper() {
        let folds = expand_window(1000, 200, 100, 100, 0);
        assert_eq!(folds.len(), 8);
    }

    #[test]
    fn test_rolling_window_helper() {
        let folds = rolling_window(1000, 200, 100, 100, 0);
        assert_eq!(folds.len(), 8);
    }

    // 用于避免 WindowType 导入未被使用
    #[test]
    fn test_window_type_variants_distinct() {
        assert_ne!(WindowType::Rolling, WindowType::Expanding);
    }
}
