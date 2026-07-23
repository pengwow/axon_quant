"""Tests for OnnxPolicyStrategy deployment adapter.

依赖 `stable_baselines3` + `onnxruntime`(`axon-quant[rl,onnx]` extra)。
缺依赖时使用 `pytest.importorskip` 跳过,确保 CI 兼容性。
"""
from __future__ import annotations

import tempfile
from pathlib import Path

import numpy as np


def _load_modules():
    """直接 load `onnx_policy.py` 及其依赖(无包 __init__ 依赖)。"""
    import importlib.util
    import sys

    def _load(name: str, path: Path):
        spec = importlib.util.spec_from_file_location(name, path)
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        return module

    base_dir = Path(__file__).parent.parent / "python" / "axon_quant" / "strategy"
    base_mod = _load("base_mod", base_dir / "base.py")
    onnx_mod = _load("onnx_policy_mod", base_dir / "onnx_policy.py")
    return base_mod, onnx_mod


def test_onnx_policy_strategy_loads_and_predicts() -> None:
    """训练 SB3 -> 导出 ONNX -> OnnxPolicyStrategy.predict 行为一致。"""
    pytest = __import__("pytest")
    pytest.importorskip("stable_baselines3")
    pytest.importorskip("onnxruntime")
    sb3 = __import__("stable_baselines3", fromlist=["PPO"])

    from axon_quant.backtest import spot_instrument
    from axon_quant.env import BacktestEnv

    # 加载 export_onnx(独立模块,无包依赖)
    import importlib.util

    export_path = (
        Path(__file__).parent.parent
        / "python"
        / "axon_quant"
        / "training"
        / "export.py"
    )
    spec = importlib.util.spec_from_file_location("export_mod", export_path)
    assert spec is not None and spec.loader is not None
    export_mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(export_mod)
    export_onnx = export_mod.export_onnx

    _, onnx_mod = _load_modules()
    OnnxPolicyStrategy = onnx_mod.OnnxPolicyStrategy

    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot, seed=42)
    model = sb3.PPO("MlpPolicy", env, verbose=0)
    model.learn(total_timesteps=10)
    obs_sample = env.observation_space.sample().astype(np.float32)
    with tempfile.TemporaryDirectory() as tmp:
        onnx_path = Path(tmp) / "policy.onnx"
        export_onnx(model, onnx_path, obs_sample)
        strategy = OnnxPolicyStrategy(
            onnx_path=onnx_path,
            leg_specs=[(spot, 1.0)],
        )
        action = strategy.predict(obs_sample)
        assert action.shape == (1,)
        assert -1.0 <= action[0] <= 1.0


def test_onnx_policy_strategy_is_base_strategy() -> None:
    """OnnxPolicyStrategy 继承 BaseStrategy。"""
    pytest = __import__("pytest")
    pytest.importorskip("onnxruntime")

    base_mod, onnx_mod = _load_modules()
    BaseStrategy = base_mod.BaseStrategy
    OnnxPolicyStrategy = onnx_mod.OnnxPolicyStrategy
    assert issubclass(OnnxPolicyStrategy, BaseStrategy)
