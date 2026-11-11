"""AXON RL 子模块 — native TradingEnv 包装 + Python 端回测评估设施。

`rl` 此前是 native-only 子模块（_native 属性直挂 + sys.modules 注册）。
2026-11-11 落地 .py 包装（同 llm/trading 模式）：re-export native 符号 +
Python 端扩展（evaluate / compute_metrics，从 QuantCell backend 下沉）。
"""

# 注意:_native 是 cdylib 单文件扩展(不是 Python package 目录),
# `from ._native.rl import ...` dot 路径不可用,需先取子模块对象再属性访问(同 llm.py)。
from axon_quant._native import rl as _native_rl  # noqa: E402

TradingEnv = _native_rl.TradingEnv

from .evaluation import compute_metrics, evaluate  # noqa: E402
from .kernel_backtest import run_kernel_backtest  # noqa: E402

__all__ = ["TradingEnv", "compute_metrics", "evaluate", "run_kernel_backtest"]
