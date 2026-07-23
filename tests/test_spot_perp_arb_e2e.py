"""D1.6 主验收 e2e:spot+perp PPO 100K 训练 + ONNX 导出 + BacktestEngine 部署。

依赖 SB3/onnxruntime(`axon-quant[rl,onnx]` extra)。
缺依赖时 `pytest.importorskip` 跳过,确保 CI 兼容性。
"""
from __future__ import annotations

import importlib.util
import tempfile
from pathlib import Path


def _load_export_onnx():
    """直接 load `python/axon_quant/training/export.py`,无包依赖。"""
    path = (
        Path(__file__).parent.parent
        / "python"
        / "axon_quant"
        / "training"
        / "export.py"
    )
    spec = importlib.util.spec_from_file_location("spot_perp_export", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    import sys
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _load_onnx_policy():
    """直接 load `python/axon_quant/strategy/onnx_policy.py` + 依赖。"""
    import sys

    def _load(name: str, path: Path):
        spec = importlib.util.spec_from_file_location(name, path)
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        return module

    base_dir = Path(__file__).parent.parent / "python" / "axon_quant" / "strategy"
    _load("base_e2e", base_dir / "base.py")
    return _load("onnx_policy_e2e", base_dir / "onnx_policy.py")


def test_spot_perp_arb_smoke_e2e() -> None:
    """Smoke e2e:10 timesteps 训练 + 导出 + 部署不崩。"""
    pytest = __import__("pytest")
    pytest.importorskip("stable_baselines3")
    pytest.importorskip("onnxruntime")
    sb3 = __import__("stable_baselines3", fromlist=["PPO"])

    from axon_quant.backtest import spot_instrument, swap_instrument
    from axon_quant.env import MultiLegBacktestEnv

    export_mod = _load_export_onnx()
    export_onnx = export_mod.export_onnx

    onnx_mod = _load_onnx_policy()
    OnnxPolicyStrategy = onnx_mod.OnnxPolicyStrategy

    spot = spot_instrument("BTC", "USDT")
    perp = swap_instrument("BTC", "USDT")
    env = MultiLegBacktestEnv([(spot, 1.0), (perp, 1.0)], seed=42)
    model = sb3.PPO("MlpPolicy", env, verbose=0)
    model.learn(total_timesteps=10)
    obs_sample = env.observation_space.sample().astype("float32")
    with tempfile.TemporaryDirectory() as tmp:
        onnx_path = Path(tmp) / "spot_perp.onnx"
        export_onnx(model, onnx_path, obs_sample)
        strategy = OnnxPolicyStrategy(
            onnx_path=onnx_path,
            leg_specs=[(spot, 1.0), (perp, 1.0)],
        )
        action = strategy.predict(obs_sample)
        assert action.shape == (2,)
        assert -1.0 <= action[0] <= 1.0
        assert -1.0 <= action[1] <= 1.0
