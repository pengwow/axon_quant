"""AXON LLM 顶层 Python API

将 Rust 端 ``axon-llm`` 的 PyO3 绑定封装为更友好的 Python 接口。

设计原则
=========

1. **配置为主**:Python 端主动通过 ``LLMConfig`` dataclass 传入 LLM 参数
   (base_url / api_key / model / max_tokens / temperature / timeout_secs),
   避免依赖环境变量(因为不同厂商 / 不同模型的变量名无法统一)。
2. **多后端支持**:`LLMConfig.backends` 是一个 list,可以同时声明多个
   LLM 后端(例如主备厂商、A/B 实验),为后续 ensemble 投票留口子。
3. **可选 retry / explain 段**:`LLMConfig.retry` 和 ``LLMConfig.explain``
   是可选 dict,缺省使用 Rust 端默认值。
4. **dict 透传**:`make_backend` 也接受原生 dict,方便从 JSON / YAML 文件
   直接反序列化后传入(对调用方更友好)。

典型用法
========

最小化用法::

    from axon_quant.llm import LLMConfig, make_backend, LLMMessage

    cfg = LLMConfig(
        backends=[{
            "name": "primary",
            "base_url": "https://api.example.com/v1",
            "api_key": "sk-xxx",
            "model": "model-name",
        }],
    )
    backend = make_backend(cfg)
    resp = backend.chat([LLMMessage("user", "你好,世界!")])
    print(resp["content"])

从 dict 直接构造::

    backend = make_backend({
        "backends": [{
            "base_url": "https://api.example.com/v1",
            "api_key": "sk-xxx",
            "model": "model-name",
            "temperature": 0.3,
        }],
        "retry": {"max_retries": 5, "initial_backoff_ms": 100},
    })

从配置文件加载::

    import tomllib
    with open("config.toml", "rb") as f:
        cfg_dict = tomllib.load(f)
    backend = make_backend(cfg_dict)
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

# 从原生 Rust 扩展导入底层类与构造函数
# 注意:`_native` 是 cdylib 单文件扩展(不是 Python package 目录),
# 所以 `from ._native.llm import ...` 这种 dot 路径不可用;
# 改用 `from axon_quant._native import llm` 先把子模块对象取出来,
# 再用属性访问取出类 / 函数。
from axon_quant._native import llm as _native_llm_module  # noqa: E402

_RustLLMBackend = _native_llm_module.LLMBackend
_RustLLMMessage = _native_llm_module.LLMMessage
_rust_make_backend = _native_llm_module.make_backend

# swarm 子模块（如果可用）
# `_native` 是 cdylib 单文件扩展(不是 Python package),所以 `axon_quant._native.llm.swarm`
# 这种 dot 路径走不通,需用 `llm.swarm` 属性访问。
try:
    _native_swarm_module = getattr(_native_llm_module, "swarm", None)
    if _native_swarm_module is not None:
        SwarmOrchestrator = _native_swarm_module.SwarmOrchestrator
        SwarmConfig = _native_swarm_module.SwarmConfig
        AgentRole = _native_swarm_module.AgentRole
        AgentStatus = _native_swarm_module.AgentStatus
        VoteType = _native_swarm_module.VoteType
        SignalType = _native_swarm_module.SignalType
        VoteProposal = _native_swarm_module.VoteProposal
        VoteResult = _native_swarm_module.VoteResult
        MarketSignal = _native_swarm_module.MarketSignal
        TradingTools = _native_swarm_module.TradingTools
        _has_swarm = True
    else:
        _has_swarm = False
except (ImportError, AttributeError):
    _has_swarm = False

__all__ = [
    "LLMConfig",
    "LLMBackend",
    "LLMMessage",
    "make_backend",
    "load_config_from_toml",
    # swarm（如果可用）
    "SwarmOrchestrator",
    "SwarmConfig",
    "AgentRole",
    "AgentStatus",
    "VoteType",
    "SignalType",
    "VoteProposal",
    "VoteResult",
    "MarketSignal",
    "TradingTools",
]


# ──────────────────────────────────────────────────────────────
# Python 端 dataclass 包装
# ──────────────────────────────────────────────────────────────


@dataclass
class LLMConfig:
    """LLM 配置 dataclass(主动传参,避免依赖环境变量)

    Attributes
    ----------
    backends : list[dict]
        后端配置列表,每个元素是包含 ``base_url`` / ``api_key`` /
        ``model`` 等字段的 dict(详见 :class:`LLMConfig` 注释)。
        当前 Rust 端 ``make_backend`` 取第一个 backend 构造实例,
        多 backend 列表为后续 ensemble 投票预留。
    retry : dict | None
        可选重试配置,字段:
        - ``max_retries`` (int,默认 3)
        - ``initial_backoff_ms`` (int,默认 200)
        - ``max_backoff_ms`` (int,默认 5000)
    explain : dict | None
        可选可解释性配置(需 Rust 端启用 ``explain`` feature),
        字段:
        - ``record_decisions`` (bool,默认 False)
        - ``store_path`` (str,默认 ``"./explain_decisions.jsonl"``)
    """

    backends: list[dict[str, Any]]
    retry: dict[str, Any] | None = None
    explain: dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        """转 dict(供 Rust 端 ``make_backend`` 消费)"""
        result: dict[str, Any] = {"backends": list(self.backends)}
        if self.retry is not None:
            result["retry"] = dict(self.retry)
        if self.explain is not None:
            result["explain"] = dict(self.explain)
        return result


# ──────────────────────────────────────────────────────────────
# 顶层 API:类型别名 + 工厂函数
# ──────────────────────────────────────────────────────────────

# 类型别名:Python 用户直接用 ``LLMBackend`` / ``LLMMessage``,不必关心 Rust 内部命名
LLMBackend = _RustLLMBackend
LLMMessage = _RustLLMMessage


def make_backend(config: LLMConfig | dict[str, Any]) -> LLMBackend:
    """构造 LLM backend 实例

    Parameters
    ----------
    config : LLMConfig | dict
        配置参数,支持 dataclass 或原生 dict。
        必填字段 ``backends`` 至少包含 1 个元素,每个元素必须有
        ``base_url`` / ``api_key`` / ``model``。

    Returns
    -------
    LLMBackend
        可用于 ``.chat([LLMMessage, ...])`` 的同步 backend 实例。

    Raises
    ------
    ValueError
        配置不合法(例如 backends 列表为空、缺少关键字段、API key 为空)。
    RuntimeError
        tokio runtime 创建失败。
    """
    if isinstance(config, LLMConfig):
        cfg_dict = config.to_dict()
    elif isinstance(config, dict):
        cfg_dict = config
    else:
        raise TypeError(f"config must be LLMConfig or dict, got {type(config).__name__}")
    # Rust 端负责详细校验(api_key 非空、URL 格式合法等)
    return _rust_make_backend(cfg_dict)


def load_config_from_toml(path: str) -> LLMConfig:
    """从 TOML 文件加载 LLMConfig

    典型配置文件::

        [[backends]]
        name = "primary"
        base_url = "https://api.example.com/v1"
        model = "model-name"
        api_key = "<set-me>"
        max_tokens = 1024
        temperature = 0.7
        timeout_secs = 60

        [retry]
        max_retries = 3
        initial_backoff_ms = 200
        max_backoff_ms = 5000

    Parameters
    ----------
    path : str
        TOML 文件路径。

    Returns
    -------
    LLMConfig
        解析后的配置对象。

    Raises
    ------
    FileNotFoundError
        文件不存在。
    ValueError
        TOML 解析失败或缺少 ``[[backends]]`` 段。
    """
    import tomllib

    with open(path, "rb") as f:
        data = tomllib.load(f)

    backends = data.get("backends")
    if not backends or not isinstance(backends, list):
        raise ValueError(f"TOML config {path} must contain a non-empty [[backends]] list")
    # tomllib 解析后 list 元素是 dict,字段名已是 str,无需再处理
    return LLMConfig(
        backends=backends,
        retry=data.get("retry"),
        explain=data.get("explain"),
    )
