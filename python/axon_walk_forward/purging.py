"""Purge / Embargo / Leakage 检测。"""

from __future__ import annotations

import numpy as np


def purge_overlapping_labels(
    train_idx: np.ndarray,
    test_idx: np.ndarray,
    label_horizon: int,
) -> np.ndarray:
    """Purge：移除训练集中与测试集标签重叠的样本。

    Args:
        train_idx: 训练集索引数组
        test_idx: 测试集索引数组
        label_horizon: 标签前瞻步数

    Returns:
        清洗后的训练集索引
    """
    if len(test_idx) == 0 or label_horizon <= 0:
        return np.asarray(train_idx).copy()
    test_start = int(test_idx.min())
    cutoff = test_start - label_horizon
    return np.asarray(train_idx)[np.asarray(train_idx) < cutoff]


def embargo_indices(
    test_idx: np.ndarray,
    embargo_pct: float,
    n_total: int,
) -> np.ndarray:
    """Embargo：在测试集之后添加隔离期。"""
    if len(test_idx) == 0 or embargo_pct <= 0.0:
        return np.array([], dtype=np.int64)
    test_end = int(test_idx.max())
    embargo_size = max(1, int(np.ceil(len(test_idx) * embargo_pct)))
    start = test_end + 1
    end = min(start + embargo_size, n_total)
    if start >= n_total:
        return np.array([], dtype=np.int64)
    return np.arange(start, end, dtype=np.int64)


def detect_leakage(
    train_idx: np.ndarray,
    test_idx: np.ndarray,
    feature_lag: int = 0,
) -> tuple[bool, list[tuple[int, int]]]:
    """检测训练集与测试集之间是否存在数据泄漏。

    Returns:
        (has_leakage, leaked_pairs)：leaked_pairs 是 (train_idx, test_idx) 元组列表
    """
    if len(train_idx) == 0 or len(test_idx) == 0:
        return False, []

    # 1. 直接索引重叠
    train_set = set(train_idx.tolist())
    test_set = set(test_idx.tolist())
    overlap = train_set & test_set
    if overlap:
        return True, [(i, i) for i in sorted(overlap)]

    # 2. 时间邻近性泄漏
    if feature_lag > 0:
        train_max = int(train_idx.max())
        test_min = int(test_idx.min())
        if test_min - train_max <= feature_lag:
            return True, [(train_max, test_min)]

    return False, []
