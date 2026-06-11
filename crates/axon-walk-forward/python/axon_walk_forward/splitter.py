"""时间序列分割器：Rolling / Expanding 窗口。"""

from __future__ import annotations

import numpy as np

from .types import FoldSplit, WalkForwardConfig, WindowType


class TimeSeriesSplitter:
    """时间序列分割器。

    关键约束：
    1. test_idx 始终 > train_idx（无未来信息）
    2. purge_gap 确保训练/测试之间无重叠
    3. embargo 机制防止训练数据与测试数据高度相关
    """

    def __init__(self, config: WalkForwardConfig):
        self.config = config

    def split(self, n_samples: int) -> list[FoldSplit]:
        """生成所有 fold 的索引分割。

        Args:
            n_samples: 总样本数

        Returns:
            FoldSplit 列表，按 fold_id 升序
        """
        cfg = self.config
        cfg.validate()
        folds: list[FoldSplit] = []

        block = cfg.train_size + cfg.validation_size + cfg.purge_gap + cfg.test_size
        if n_samples < block:
            return folds

        step_pos = block
        fold_id = 0
        while step_pos <= n_samples:
            test_end = step_pos
            test_start = test_end - cfg.test_size
            val_end = test_start - cfg.purge_gap
            val_start = val_end - cfg.validation_size
            train_end = val_start
            if cfg.window_type == WindowType.EXPANDING:
                train_start = 0
            else:  # ROLLING
                train_start = max(0, train_end - cfg.train_size)

            if train_start > train_end:
                break

            folds.append(
                FoldSplit(
                    fold_id=fold_id,
                    train_start=train_start,
                    train_end=train_end,
                    validation_start=val_start,
                    validation_end=val_end,
                    test_start=test_start,
                    test_end=test_end,
                )
            )
            fold_id += 1
            step_pos += cfg.step_size

        return folds

    def split_indices(self, n_samples: int) -> list[tuple[np.ndarray, np.ndarray, np.ndarray]]:
        """返回 numpy 数组形式的 (train_idx, val_idx, test_idx) 列表。"""
        return [
            (
                np.arange(f.train_start, f.train_end),
                np.arange(f.validation_start, f.validation_end),
                np.arange(f.test_start, f.test_end),
            )
            for f in self.split(n_samples)
        ]


def expand_window(
    n_samples: int,
    train_size: int,
    test_size: int,
    step_size: int,
    purge_gap: int = 0,
) -> list[FoldSplit]:
    """便捷函数：扩展窗口。"""
    cfg = WalkForwardConfig.expanding(train_size, test_size, step_size)
    cfg.purge_gap = purge_gap
    return TimeSeriesSplitter(cfg).split(n_samples)


def rolling_window(
    n_samples: int,
    train_size: int,
    test_size: int,
    step_size: int,
    purge_gap: int = 0,
) -> list[FoldSplit]:
    """便捷函数：滚动窗口。"""
    cfg = WalkForwardConfig.rolling(train_size, test_size, step_size)
    cfg.purge_gap = purge_gap
    return TimeSeriesSplitter(cfg).split(n_samples)
