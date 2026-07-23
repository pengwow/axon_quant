"""Tests for SB3 policy -> ONNX export.

依赖 `stable_baselines3` + `onnxruntime`(`axon-quant[rl,onnx]` extra)。
缺依赖时使用 `pytest.importorskip` 跳过,确保 CI 兼容性。
"""
from __future__ import annotations

import tempfile
from pathlib import Path

import numpy as np


def _load_export_onnx():
    """直接 load `python/axon_quant/training/export.py`,不依赖包 __init__。"""
    import importlib.util
    import sys

    path = (
        Path(__file__).parent.parent
        / "python"
        / "axon_quant"
        / "training"
        / "export.py"
    )
    spec = importlib.util.spec_from_file_location("export_mod", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_export_onnx_creates_file() -> None:
    """export_onnx 写入 .onnx 文件,文件可被 onnxruntime 加载。"""
    pytest = __import__("pytest")
    pytest.importorskip("stable_baselines3")
    pytest.importorskip("onnxruntime")
    sb3 = __import__("stable_baselines3", fromlist=["PPO"])
    ort = pytest.importorskip("onnxruntime")

    from axon_quant.backtest import spot_instrument
    from axon_quant.env import BacktestEnv

    export_mod = _load_export_onnx()
    export_onnx = export_mod.export_onnx

    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot, seed=42)
    model = sb3.PPO("MlpPolicy", env, verbose=0)
    model.learn(total_timesteps=10)  # smoke
    obs_sample = env.observation_space.sample()
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "policy.onnx"
        returned = export_onnx(model, out, obs_sample)
        assert returned == out
        assert out.exists()
        # onnxruntime 可加载
        sess = ort.InferenceSession(str(out))
        assert sess.get_inputs()[0].name == "obs"


def test_export_onnx_predict_consistent() -> None:
    """export_onnx 后,onnxruntime predict 与 SB3 predict 一致(smoke)。"""
    pytest = __import__("pytest")
    pytest.importorskip("stable_baselines3")
    pytest.importorskip("onnxruntime")
    sb3 = __import__("stable_baselines3", fromlist=["PPO"])
    ort = pytest.importorskip("onnxruntime")

    from axon_quant.backtest import spot_instrument
    from axon_quant.env import BacktestEnv

    export_mod = _load_export_onnx()
    export_onnx = export_mod.export_onnx

    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot, seed=42)
    model = sb3.PPO("MlpPolicy", env, verbose=0)
    model.learn(total_timesteps=10)
    obs_sample = env.observation_space.sample().astype(np.float32)
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "policy.onnx"
        export_onnx(model, out, obs_sample)
        # onnxruntime predict
        sess = ort.InferenceSession(str(out))
        onnx_out = sess.run(None, {"obs": obs_sample.reshape(1, -1)})[0]
        # SB3 predict
        sb3_out, _ = model.predict(obs_sample, deterministic=True)
        # 允许 1e-3 浮点误差
        np.testing.assert_allclose(onnx_out, sb3_out, atol=1e-3)
