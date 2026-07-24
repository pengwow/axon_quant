"""L4 perf test:100 trial 8-CPU HPO sweep wall-time <= 3h (D1.5c)。

标记 @slow,默认 CI 跳过(运行: pytest --slow)。
"""
from __future__ import annotations

import time

import pytest


@pytest.mark.slow
def test_hpo_100_trials_8cpu_under_3h() -> None:
    """100 trial 8-CPU 并发 sweep wall-time 应 <= 3h。

    接受标准:10800s 内完成。
    依赖:optuna + stable_baselines3 + axon_hpo,缺依赖时自动 skip。
    """
    pytest.importorskip("optuna")
    pytest.importorskip("stable_baselines3")
    pytest.importorskip("axon_hpo")
    sb3 = __import__("stable_baselines3", fromlist=["PPO"])

    from axon_quant.backtest import spot_instrument
    from axon_quant.env import BacktestEnv
    from axon_quant.training.hpo_sweeper import RLHPOSweeper

    sweeper = RLHPOSweeper(
        study_name="perf_hpo_test",
        n_trials=100,
        storage="sqlite:///optuna_perf_test.db",
        n_jobs=8,
    )

    def objective(params):
        env = BacktestEnv(spot_instrument("BTC", "USDT"), seed=42)
        model = sb3.PPO("MlpPolicy", env, verbose=0, **params)
        model.learn(total_timesteps=50_000)
        return [1.0]

    start = time.time()
    best = sweeper.sweep(objective_fn=objective)
    wall_time_sec = time.time() - start

    assert "lr" in best
    assert wall_time_sec <= 3 * 3600, f"wall time {wall_time_sec:.1f}s > 3h"
