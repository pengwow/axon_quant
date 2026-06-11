"""多目标优化：Pareto 前沿与超体积计算。

Rust 端 `axon_hpo::pareto` 提供权威实现。本模块提供 Python 版本，
用于在 Python 端快速分析 Optuna 输出。
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Sequence

import numpy as np

from .types import StudyDirection, TrialResult


@dataclass
class ParetoPoint:
    """Pareto 前沿上的一个点。"""

    params: dict
    objectives: list[float]
    trial_id: int


def dominates(
    a: Sequence[float], b: Sequence[float], directions: Sequence[StudyDirection]
) -> bool:
    """判断 a 是否 Pareto 支配 b。

    支配条件（针对 `directions`）：
    - a 在每个目标上 >= b（按 direction 调整）
    - a 至少在一个目标上 > b
    """
    if len(a) != len(b) or len(a) != len(directions):
        return False
    at_least_one_better = False
    for val_a, val_b, d in zip(a, b, directions):
        if d.is_maximize:
            if val_a < val_b:
                return False
            if val_a > val_b:
                at_least_one_better = True
        else:
            if val_a > val_b:
                return False
            if val_a < val_b:
                at_least_one_better = True
    return at_least_one_better


def compute_pareto_front(
    trials: list[TrialResult], directions: Sequence[StudyDirection]
) -> list[ParetoPoint]:
    """计算 Pareto 前沿。

    Args:
        trials: 所有 trial 结果
        directions: 每个目标的优化方向

    Returns:
        Pareto 前沿点列表
    """
    if not directions:
        raise ValueError("directions 不能为空")
    if not trials:
        return []

    # 过滤有效 trial（state == complete 且 values 长度匹配）
    valid = [
        t for t in trials if t.state == "complete" and len(t.values) == len(directions)
    ]
    if not valid:
        return []

    front: list[ParetoPoint] = []
    for i, t_i in enumerate(valid):
        dominated = False
        for j, t_j in enumerate(valid):
            if i == j:
                continue
            if dominates(t_j.values, t_i.values, directions):
                dominated = True
                break
        if not dominated:
            front.append(
                ParetoPoint(
                    params=t_i.params,
                    objectives=t_i.values,
                    trial_id=t_i.trial_id,
                )
            )
    return front


def compute_hypervolume(
    front: list[ParetoPoint],
    directions: Sequence[StudyDirection],
    reference_point: Sequence[float],
) -> float:
    """计算超体积（Hypervolume Indicator）。

    2D：精确计算（排序后梯形面积）
    N-D：近似计算（参考点减去最差前沿点之积）
    """
    if not front:
        return 0.0
    if len(reference_point) != len(directions):
        raise ValueError(
            f"reference_point 长度 ({len(reference_point)}) 必须等于 "
            f"directions 长度 ({len(directions)})"
        )

    n_obj = len(directions)
    objectives = np.array([p.objectives for p in front], dtype=np.float64)
    ref = np.array(reference_point, dtype=np.float64)

    # 2D 精确（仅 maximize + maximize 走快路径）
    if n_obj == 2 and directions[0].is_maximize and directions[1].is_maximize:
        sorted_idx = np.argsort(objectives[:, 0])
        sorted_objs = objectives[sorted_idx]
        hv = 0.0
        for obj in sorted_objs:
            width = max(0.0, ref[0] - obj[0])
            height = max(0.0, ref[1] - obj[1])
            hv += width * height
        return float(hv)

    # N-D 近似
    min_per_dim = ref.copy()
    for obj in objectives:
        for d in range(n_obj):
            if obj[d] < min_per_dim[d]:
                min_per_dim[d] = obj[d]
    widths = np.maximum(0.0, ref - min_per_dim)
    return float(np.prod(widths))


def select_by_constraint(
    front: list[ParetoPoint],
    constraint_fn,
) -> ParetoPoint | None:
    """从 Pareto 前沿按约束选取最佳点。

    Args:
        front: Pareto 前沿
        constraint_fn: 约束函数，接收 objectives 返回 bool

    Returns:
        满足约束的第一个点，没有则返回 None
    """
    for point in front:
        if constraint_fn(point.objectives):
            return point
    return None
