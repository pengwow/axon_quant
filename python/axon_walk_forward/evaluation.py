"""Walk-Forward 指标聚合与稳定性分析。"""

from __future__ import annotations

import numpy as np

from .types import (
    AggregatedMetrics,
    FoldResult,
    StabilityMetrics,
)


def _mean(xs: list[float]) -> float:
    return float(np.mean(xs)) if xs else 0.0


def _std(xs: list[float]) -> float:
    if len(xs) < 2:
        return 0.0
    return float(np.std(xs, ddof=1))


def _median(xs: list[float]) -> float:
    return float(np.median(xs)) if xs else 0.0


def _deflated_sharpe(observed: float, n_trials: int, sharpe_std: float) -> float:
    """Deflated Sharpe Ratio（Bailey & López de Prado, 2014）。"""
    if abs(sharpe_std) < 1e-9 or n_trials == 0:
        return 0.0
    euler_gamma = 0.5772156649015329
    log_n = max(np.log(max(n_trials, 1)), 1.0)
    sqrt_2_log_n = np.sqrt(2.0 * log_n)
    e_max = sqrt_2_log_n * (1.0 - euler_gamma / (2.0 * log_n)) + euler_gamma / (
        2.0 * sqrt_2_log_n
    )
    z = (observed - e_max) / sharpe_std
    return float(_norm_cdf(z))


def _norm_cdf(z: float) -> float:
    """标准正态 CDF 近似。"""
    return 0.5 * (1.0 + _erf(z / np.sqrt(2.0)))


def _erf(x: float) -> float:
    """Abramowitz & Stegun 7.1.26 近似。"""
    a1, a2, a3, a4, a5 = (
        0.254829592,
        -0.284496736,
        1.421413741,
        -1.453152027,
        1.061405429,
    )
    p = 0.3275911
    sign = 1.0 if x >= 0 else -1.0
    x = abs(x)
    t = 1.0 / (1.0 + p * x)
    y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * np.exp(-x * x)
    return sign * y


def aggregate_folds(
    folds: list[FoldResult],
) -> tuple[AggregatedMetrics, StabilityMetrics]:
    """聚合所有 fold 的结果。"""
    if not folds:
        return AggregatedMetrics(), StabilityMetrics()

    test_returns = [f.test_return for f in folds]
    test_sharpes = [f.test_sharpe for f in folds]

    agg = AggregatedMetrics(
        mean_oos_return=_mean(test_returns),
        std_oos_return=_std(test_returns),
        mean_oos_sharpe=_mean(test_sharpes),
        std_oos_sharpe=_std(test_sharpes),
        median_oos_return=_median(test_returns),
        worst_fold_return=float(min(test_returns)),
        best_fold_return=float(max(test_returns)),
        pct_profitable_folds=sum(1 for r in test_returns if r > 0) / len(test_returns),
    )

    sharpe_std = _std(test_sharpes)
    sharpe_of_sharpe = (
        _mean(test_sharpes) / sharpe_std if sharpe_std > 1e-9 else 0.0
    )

    if len(test_returns) > 2:
        prev = np.array(test_returns[:-1])
        curr = np.array(test_returns[1:])
        if prev.std() > 1e-9 and curr.std() > 1e-9:
            autocorr = float(np.corrcoef(prev, curr)[0, 1])
        else:
            autocorr = 0.0
    else:
        autocorr = 0.0

    deflated = _deflated_sharpe(_mean(test_sharpes), len(test_sharpes), sharpe_std)

    # 下一 fold 亏损概率：
    # P(loss) = 1 - Φ((0 - mean) / std) = Φ(mean / std)
    ret_std = _std(test_returns)
    if len(test_returns) > 1 and ret_std > 1e-9:
        z = _mean(test_returns) / ret_std
        prob_loss = 1.0 - _norm_cdf(z)
    else:
        prob_loss = 0.5

    stab = StabilityMetrics(
        sharpe_of_sharpe=sharpe_of_sharpe,
        return_autocorrelation=autocorr,
        deflated_sharpe=deflated,
        probability_of_loss=prob_loss,
    )

    return agg, stab
