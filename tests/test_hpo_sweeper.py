"""Tests for RL HPO sweeper glue (axon-hpo OptunaHPO).

依赖 `axon_hpo` + `optuna` + `stable_baselines3`。
缺依赖时使用 `pytest.importorskip` 跳过,确保 CI 兼容性。
"""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path


def _load_hpo_sweeper():
    """直接 load `python/axon_quant/training/hpo_sweeper.py`,无包 __init__ 依赖。"""
    path = (
        Path(__file__).parent.parent
        / "python"
        / "axon_quant"
        / "training"
        / "hpo_sweeper.py"
    )
    spec = importlib.util.spec_from_file_location("hpo_sweeper_mod", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_default_search_space_has_required_keys() -> None:
    """默认搜索空间包含 lr / gamma / clip_param / entropy_coeff。

    缺 axon_hpo 时跳过(填充函数抛 ModuleNotFoundError)。
    """
    pytest = __import__("pytest")
    pytest.importorskip("axon_hpo")

    mod = _load_hpo_sweeper()
    # 触发延迟填充
    mod._ensure_axon_hpo()
    space = mod.DEFAULT_SEARCH_SPACE
    for k in ("lr", "gamma", "clip_param", "entropy_coeff"):
        assert k in space, f"missing key {k!r} in DEFAULT_SEARCH_SPACE"


def test_rl_hpo_sweeper_smoke_2_trials() -> None:
    """小规模 2 trial smoke test 跑通。"""
    pytest = __import__("pytest")
    pytest.importorskip("axon_hpo")
    pytest.importorskip("optuna")
    pytest.importorskip("stable_baselines3")
    sb3 = __import__("stable_baselines3", fromlist=["PPO"])

    from axon_quant.backtest import spot_instrument
    from axon_quant.env import BacktestEnv

    mod = _load_hpo_sweeper()
    RLHPOSweeper = mod.RLHPOSweeper

    def objective(params: dict) -> list[float]:
        spot = spot_instrument("BTC", "USDT")
        env = BacktestEnv(spot, seed=42)
        model = sb3.PPO("MlpPolicy", env, verbose=0, **params)
        model.learn(total_timesteps=50)
        return [50.0]  # 占位 reward

    sweeper = RLHPOSweeper(
        study_name="test_rl_hpo",
        n_trials=2,
        storage=None,  # in-memory
    )
    best = sweeper.sweep(objective_fn=objective)
    assert "lr" in best
