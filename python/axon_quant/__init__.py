"""AXON Quant - 量化交易回测与强化学习框架

Rust 核心 + Python RL 接口，从回测到生产的全链路统一框架。

子模块：
- ``rl`` — Gymnasium 兼容的 RL 交易环境（TradingEnv / VecEnv）
- ``hpo`` — 超参数优化（Optuna 集成 / 多目标 / 剪枝）
- ``walk_forward`` — 滚动前向验证（purge / embargo / 泄漏检测）
- ``tracker`` — 实验追踪（Memory / Local / MLflow / WandB）
- ``registry`` — 模型注册表（版本管理 / 生命周期 / 本地存储）
- ``distributed`` — 分布式训练（Ray / 参数服务器 / 检查点）

用法::

    import axon_quant

    env = axon_quant.rl.TradingEnv(
        config={"initial_capital": 100_000.0, "max_steps": 1000},
        action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    )
"""

from __future__ import annotations

# 从原生 Rust 扩展导入所有符号
from ._native import *  # noqa: F401, F403
from ._native import __version__  # noqa: F401

# 重新导出原生子模块（由 Rust PyO3 注册）
from ._native import rl, tracker, registry, hpo, walk_forward, distributed  # noqa: F401

__all__ = [
    "__version__",
    "rl",
    "hpo",
    "walk_forward",
    "tracker",
    "registry",
    "distributed",
]
