"""rl.evaluation 评估设施验证（2026-11-11 从 QuantCell backend/rl/evaluation.py 下沉）。

假 env 完全确定性（无随机），覆盖：
1) 全轨迹指标数值（total_pnl / sharpe / max_drawdown / win_rate / profit_factor / turnover）；
2) profit_factor=999.99 哨兵（有盈利、无亏损轨迹）；
3) 轨迹过短（<2 个 NAV 点）时的零值 dict（对齐原 evaluate_model 早退 EvaluationMetrics()）；
4) turnover 反推：cum transaction_costs / transaction_cost，费率 0 记 0.0。

数值口径逐行对齐 backend 原实现：
- sharpe：returns 经 nan_to_num 后，仅 len>1 且 std>0 才算，√252 年化、ddof=0、round 4；
- max_drawdown_pct：-max(drawdowns)×100，符号为负；
- profit_factor：基于收益率（diff/nav[:-1]），非 NAV 增量；无亏损且盈利 → 999.99 哨兵。
"""

import math

import numpy as np
import pytest

from axon_quant.rl.evaluation import compute_metrics, evaluate


class _FakeEnv:
    """Gymnasium 风格确定性 stub。

    NAV = initial × Π(1 + r_i × action)；每步累计 transaction_costs = step_cost × 已走步数。
    """

    def __init__(self, initial=100.0, returns=(), step_cost=0.0):
        self._initial = initial
        self._returns = list(returns)
        self._step_cost = step_cost
        self._offset = 0  # 给 predict_fn 一个可区分的假观测

    def _observation(self):
        self._offset += 1
        return [float(self._offset)]

    def reset(self, *, seed=None, options=None):
        self._i = 0
        self._nav = self._initial
        return self._observation(), {"portfolio_value": self._nav}

    def step(self, action):
        if self._i >= len(self._returns):
            # 空 episode：无可用 step，Gymnasium 语义下立即终止
            return self._observation(), 0.0, True, False, {
                "portfolio_value": self._nav,
                "trades_executed": 0,
                "transaction_costs": 0.0,
            }
        r = self._returns[self._i] * float(action)
        self._nav *= 1.0 + r
        self._i += 1
        info = {
            "portfolio_value": self._nav,
            "trades_executed": self._i,
            "transaction_costs": self._step_cost * self._i,
        }
        return self._observation(), 0.0, self._i >= len(self._returns), False, info


def test_evaluate_full_trajectory_metrics():
    # NAV: 100 → 110 → 104.5 → 106.59；action 恒 1.0
    env = _FakeEnv(initial=100.0, returns=[0.10, -0.05, 0.02], step_cost=0.001)
    raw = evaluate(lambda obs: 1.0, env, transaction_cost=0.001)

    assert raw["total_pnl"] == pytest.approx(6.59, abs=1e-2)          # round 2
    assert raw["total_return_pct"] == pytest.approx(6.59, abs=1e-2)   # 同比 %，round 4
    # sharpe 与 backend 原口径一致：√252 年化、ddof=0、round 4（原实现有 std>0 才算的 guard）
    ret = np.array([0.10, -0.05, 0.02])
    expected_sharpe = round(float(ret.mean() / ret.std() * math.sqrt(252)), 4)
    assert raw["sharpe_ratio"] == pytest.approx(expected_sharpe, abs=1e-6)
    # backend 原口径：-max(drawdowns)×100，负号；峰值 110 → 谷值 104.5 → -5.0
    assert raw["max_drawdown_pct"] == pytest.approx(-5.0, abs=1e-2)
    assert raw["win_rate"] == pytest.approx(round(2 / 3, 4), abs=1e-4)  # 收益率 >0 的 step 占比
    assert raw["num_trades"] == 3
    # backend 原口径：profit_factor 基于收益率 (0.1+0.02)/0.05 = 2.4，非 NAV 增量
    assert raw["profit_factor"] == pytest.approx(2.4, abs=1e-6)
    assert raw["turnover"] == pytest.approx(3.0, abs=1e-9)            # 0.003 / 0.001


def test_profit_factor_sentinel_when_no_losses():
    env = _FakeEnv(initial=100.0, returns=[0.01, 0.01], step_cost=0.0)
    raw = evaluate(lambda obs: 1.0, env, transaction_cost=0.0)
    assert raw["profit_factor"] == 999.99
    assert raw["turnover"] == 0.0  # 费率无效 → 反推记 0.0，不除零


def test_short_trajectory_returns_zero_metrics():
    env = _FakeEnv(initial=100.0, returns=[], step_cost=0.0)
    raw = evaluate(lambda obs: 1.0, env, transaction_cost=0.0)
    assert raw["total_pnl"] == 0.0
    assert raw["num_trades"] == 0
    assert raw["turnover"] == 0.0
    # 其余统计字段同 EvaluationMetrics() 默认口径（backend 原实现的早退默认实例）
    assert raw["total_return_pct"] == 0.0
    assert raw["sharpe_ratio"] == 0.0
    assert raw["max_drawdown_pct"] == 0.0
    assert raw["win_rate"] == 0.0
    assert raw["profit_factor"] == 0.0
    # 单 NAV 点直调：命中 <2 早退，与原 evaluate_model 全零默认实例同口径
    # （早退忽略传入的 num_trades/turnover，与原实现 EvaluationMetrics() 一致）
    raw2 = compute_metrics([100.0], num_trades=7, turnover=9.9)
    assert raw2["total_pnl"] == 0.0
    assert raw2["total_return_pct"] == 0.0
    assert raw2["sharpe_ratio"] == 0.0
    assert raw2["max_drawdown_pct"] == 0.0
    assert raw2["win_rate"] == 0.0
    assert raw2["num_trades"] == 0
    assert raw2["profit_factor"] == 0.0
    assert raw2["turnover"] == 0.0


def test_compute_metrics_pure_stats():
    # 直接驱动指标核心：两行 NAV，验证 round(%)
    raw = compute_metrics([100.0, 106.59], num_trades=1, turnover=0.5)
    assert raw["total_pnl"] == 6.59
    assert raw["total_return_pct"] == pytest.approx(round(6.59, 4), abs=1e-6)
    assert raw["turnover"] == 0.5  # 不 round
