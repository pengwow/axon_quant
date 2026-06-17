"""AXON Quant - 量化交易回测与强化学习框架

Rust 核心 + Python RL 接口，从回测到生产的全链路统一框架。

子模块：
- ``rl`` — Gymnasium 兼容的 RL 交易环境（TradingEnv / VecEnv）
- ``hpo`` — 超参数优化（Optuna 集成 / 多目标 / 剪枝）
- ``walk_forward`` — 滚动前向验证（purge / embargo / 泄漏检测）
- ``tracker`` — 实验追踪（Memory / Local / MLflow / WandB）
- ``registry`` — 模型注册表（版本管理 / 生命周期 / 本地存储）
- ``distributed`` — 分布式训练（Ray / 参数服务器 / 检查点）
- ``llm`` — LLM 后端（OpenAI 兼容协议，多厂商主备，PyO3 绑定）

用法::

    import axon_quant

    env = axon_quant.rl.TradingEnv(
        config={"initial_capital": 100_000.0, "max_steps": 1000},
        action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    )

    # LLM 后端:主动传参,避免依赖环境变量
    from axon_quant.llm import LLMConfig, make_backend, LLMMessage
    backend = make_backend(LLMConfig(backends=[{
        "base_url": "https://api.example.com/v1",
        "api_key": "sk-xxx",
        "model": "model-name",
    }]))
    print(backend.chat([LLMMessage("user", "Hi!")])["content"])
"""

from __future__ import annotations

# 从原生 Rust 扩展导入所有符号
from ._native import *  # noqa: F401, F403

# 重新导出原生子模块（由 Rust PyO3 注册）
# 注意:`llm` 是从纯 Python `axon_quant.llm` 模块导出(见下方),
# 因为我们对它做了 Python 端的封装(LLMConfig / make_backend / load_config_from_toml),
# 不直接 re-export 原生的 `_native.llm`。
from ._native import (  # noqa: F401
    __version__,  # noqa: F401
    distributed,
    hpo,
    registry,
    rl,
    tracker,
    walk_forward,
)

# 重新导出 LLM 顶层 Python API(包装 _native.llm)
# 这里必须用 `from .llm import ...` 而非 `from . import llm`,
# 后者会优先复用 sys.modules['axon_quant.llm'] 缓存,
# 而该缓存可能已经被 `from ._native import *` 注入为原生 _native.llm 引用。
from .llm import (  # noqa: F401
    LLMBackend,
    LLMConfig,
    LLMMessage,
    load_config_from_toml,
    make_backend,
)

# 让 `axon_quant.llm` 这个子模块也对外可见(给文档 / 静态分析使用)
# noqa: F405 是因为 ruff 误判 llm 来自 `from ._native import *`,
# 实际上下方 `from .llm import ...` 并没有显式 import `llm` 这个模块对象
__all__ = [  # noqa: F405
    "__version__",
    "rl",
    "hpo",
    "walk_forward",
    "tracker",
    "registry",
    "distributed",
    "llm",
    "LLMConfig",
    "LLMBackend",
    "LLMMessage",
    "make_backend",
    "load_config_from_toml",
]
