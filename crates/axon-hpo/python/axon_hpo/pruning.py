"""剪枝策略。

Optuna 自带的剪枝器（Median / Hyperband / SuccessiveHalving）通常够用。
本模块提供自定义剪枝策略（如分位数剪枝）的占位扩展点。
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import optuna


def adaptive_median_prune(
    trial: "optuna.Trial",
    step: int,
    value: float,
    n_startup_trials: int = 5,
    n_warmup_steps: int = 10,
    percentile: float = 50.0,
) -> bool:
    """自适应中位数剪枝：前 N 个 trial 不剪枝，前 M 步不剪枝，之后按分位数剪。

    Args:
        trial: 当前 Optuna trial
        step: 当前报告的步
        value: 当前报告的值
        n_startup_trials: 启动 trial 数（trial.number < 此值不剪枝）
        n_warmup_steps: 预热步数（step < 此值不剪枝）
        percentile: 分位数阈值（value 低于同 study 已完成 trial 的分位数则剪枝）

    Returns:
        True 表示应剪枝
    """
    if trial.number < n_startup_trials:
        return False
    if step < n_warmup_steps:
        return False

    try:
        import numpy as np  # noqa: PLC0415

        completed = [
            t.values[0]
            for t in trial.study.trials
            if t.state.name == "COMPLETE" and t.values
        ]
        if len(completed) < n_startup_trials:
            return False
        threshold = np.percentile(completed, percentile)
        return value < threshold
    except ImportError:
        # 无 numpy 时退化为恒真
        return False
