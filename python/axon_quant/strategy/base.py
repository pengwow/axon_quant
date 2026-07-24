"""BaseStrategy 抽象(0.9.0 C3.1)。

镜像 Rust `StreamingStrategy` trait:
- `on_bar(bar, ctx) -> list[OrderDict]`:每 bar 决策
- `on_fill(fill, ctx) -> list[OrderDict]`:每 fill 决策(默认 no-op)
"""
from __future__ import annotations

from abc import ABC, abstractmethod
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from axon_quant.backtest import FillDict, OrderDict


class BaseStrategy(ABC):
    """多 leg 策略抽象(镜像 Rust 侧 `StreamingStrategy`)。

    0.9.0 简化:
    - 不强求多 leg 抽象对称(单 leg `BaseStrategy` 也能用,单 leg 用
      `on_bar(instrument=...)` 透传)
    - Python 侧类型注解都走 TYPE_CHECKING,避免 import 循环
    """

    @abstractmethod
    def on_bar(self, bar: Any, ctx: Any) -> list["OrderDict"]:
        """每 bar 调用,返回应执行的订单列表。"""

    def on_fill(self, fill: "FillDict", ctx: Any) -> list["OrderDict"]:
        """每 fill 调用(默认 no-op)。"""
        return []
