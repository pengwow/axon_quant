"""Smoke tests for CartPole training example (D1.3a, 0.9.0).

不依赖真实 ray/RLLib:验证 train_cartpole.py 在 mock 模式下行为正确
(API 集成 OK,build_algo 返回 None,converged=False)。
真实 5K 收敛需要在装好 `axon-distributed` + `ray[rllib]` + `torch` 后
手动跑 `uv run python examples/rl/train_cartpole.py`。
"""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

# axon_distributed 是 monorepo 兄弟 crate,未发布到 PyPI;
# 测试时临时把它的 Python 源码目录加入 sys.path。
_AXON_DISTRIBUTED_PYTHON = (
    Path(__file__).parent.parent
    / "crates"
    / "axon-distributed"
    / "python"
)
if str(_AXON_DISTRIBUTED_PYTHON) not in sys.path:
    sys.path.insert(0, str(_AXON_DISTRIBUTED_PYTHON))


def _load_train_cartpole():
    """直接 load examples/rl/train_cartpole.py(examples 不是 Python package)。"""
    path = Path(__file__).parent.parent / "examples" / "rl" / "train_cartpole.py"
    spec = importlib.util.spec_from_file_location("train_cartpole", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_train_cartpole_mock_mode_returns_false() -> None:
    """Mock 模式(init_ray=False)build_algo 返回 None,converged=False。

    验证:
    - import 路径正确(examples/rl/train_cartpole.py 可被加载)
    - DistributedTrainer mock 模式集成 OK(API 签名匹配)
    - 不依赖真实 ray 环境
    """
    train_cartpole = _load_train_cartpole()
    converged = train_cartpole.train_cartpole(init_ray=False)
    assert converged is False


def test_train_cartpole_constants() -> None:
    """train_cartpole 模块导出 CartPole 收敛阈值常量。"""
    module = _load_train_cartpole()
    # 常量值符合 CartPole-v1 经典收敛阈值
    assert module.CARTPOLE_REWARD_THRESHOLD == 475
    assert module.CARTPOLE_MAX_ITERATIONS == 10
