"""Tests for BaseStrategy ABC (mirror of Rust StreamingStrategy trait)."""
from __future__ import annotations

import pytest

from axon_quant.backtest import limit_order, spot_instrument
from axon_quant.strategy.base import BaseStrategy


def test_base_strategy_cannot_be_instantiated() -> None:
    """BaseStrategy 是 ABC,不能直接实例化。"""
    with pytest.raises(TypeError):
        BaseStrategy()  # type: ignore[abstract]


def test_subclass_must_implement_on_bar() -> None:
    """子类必须实现 on_bar(否则仍是 ABC)。"""

    class IncompleteStrategy(BaseStrategy):
        pass

    with pytest.raises(TypeError):
        IncompleteStrategy()  # type: ignore[abstract]


def test_complete_subclass_can_be_instantiated() -> None:
    """完整子类可实例化。"""

    class SimpleStrategy(BaseStrategy):
        def on_bar(self, bar, ctx):
            spot = spot_instrument("BTC", "USDT")
            return [limit_order(1, spot, "Buy", 100.0, 1.0)]

    strat = SimpleStrategy()
    spot = spot_instrument("BTC", "USDT")
    orders = strat.on_bar(None, ctx=None)  # type: ignore[arg-type]
    assert len(orders) == 1
    assert orders[0]["side"] == "Buy"


def test_on_fill_default_returns_empty() -> None:
    """on_fill 默认 no-op(返回 [])。"""

    class SimpleStrategy(BaseStrategy):
        def on_bar(self, bar, ctx):
            return []

    strat = SimpleStrategy()
    assert strat.on_fill(None, ctx=None) == []  # type: ignore[arg-type]
