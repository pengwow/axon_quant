"""Integration test: spot single leg PPO 50 step smoke (D1.3b).

完整 50K PPO 训练示例在 `examples/rl/train_spot_single_leg.py`。
本测试只跑 50 step 烟雾,验证 SB3 + BacktestEnv 集成 OK,
不依赖 50K 真实训练(那需要几分钟 CPU 时间)。
"""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path


def _load_train_spot_single_leg():
    """直接 load examples/rl/train_spot_single_leg.py。"""
    path = (
        Path(__file__).parent.parent
        / "examples"
        / "rl"
        / "train_spot_single_leg.py"
    )
    spec = importlib.util.spec_from_file_location("train_spot_single_leg", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_spot_single_leg_constants() -> None:
    """train_spot_single_leg 导出 50K timesteps 默认值。

    用 AST 解析模块源码,避免在缺 `stable_baselines3` 依赖时
    顶层 import 报错导致常量测试被误失败。
    """
    import ast

    path = (
        Path(__file__).parent.parent
        / "examples"
        / "rl"
        / "train_spot_single_leg.py"
    )
    source = path.read_text()
    tree = ast.parse(source)

    constants = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            target = node.targets[0]
            if isinstance(target, ast.Name) and target.id.isupper():
                # 简化:只支持字面量(int / str);其他表达式(ast.BinOp 等)跳
                if isinstance(node.value, ast.Constant) and isinstance(
                    node.value.value, (int, str)
                ):
                    constants[target.id] = node.value.value

    assert constants.get("TOTAL_TIMESTEPS") == 50_000
    # MODEL_PATH / TB_LOG_DIR 是 Path 表达式,这里用源码包含字符串验证即可
    assert '"spot_single_leg_ppo.zip"' in source
    assert '"./tb_logs/spot_single_leg/"' in source


def test_spot_single_leg_ppo_smoke_trains() -> None:
    """50 step PPO 训练不崩(快速 smoke)。

    依赖:`stable-baselines3` + `torch`(在 `axon-quant[rl]` extra)。
    """
    pytest = __import__("pytest")
    sb3 = pytest.importorskip("stable_baselines3")
    np = __import__("importlib").import_module("numpy")

    from axon_quant.backtest import spot_instrument
    from axon_quant.env import BacktestEnv

    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot, seed=42)
    model = sb3.PPO("MlpPolicy", env, verbose=0)
    model.learn(total_timesteps=50)  # smoke:50 而非 50K

    # 训练后 predict 不崩
    obs, _ = env.reset(seed=42)
    action, _ = model.predict(obs, deterministic=True)
    assert action.shape == env.action_space.shape
    # action 范围应在 [-1, 1](gym.Env 约束)
    assert -1.0 <= float(action[0]) <= 1.0
    _ = np  # 避免未使用
