"""axon_quant.training — 训练工具层（0.9.0 D1.4/D1.5）。

- export：SB3 policy → ONNX 导出
- hpo_sweeper：基于 Optuna 的 RL HPO 胶水

两个符号都重依赖（torch / optuna），故用 ``__getattr__`` 延迟导入，
保证 ``import axon_quant.training`` 本身不强制引入这些可选依赖。
"""
from __future__ import annotations

__all__ = ["export_onnx", "RLHPOSweeper"]


def __getattr__(name: str):
    if name == "export_onnx":
        from .export import export_onnx

        return export_onnx
    if name == "RLHPOSweeper":
        from .hpo_sweeper import RLHPOSweeper

        return RLHPOSweeper
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")