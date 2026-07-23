"""axon_quant.strategy — 策略抽象层(0.9.0 C3.1)。

镜像 Rust 侧 `axon_backtest::streaming::StreamingStrategy` trait,
提供 Python 端多 leg 策略接口。
"""
from axon_quant.strategy.base import BaseStrategy

__all__ = ["BaseStrategy"]
