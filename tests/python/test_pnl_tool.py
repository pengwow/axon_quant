"""GetPnlTool Python API 测试

覆盖:
- GetPnlTool 构造 + 默认配置
- get_pnl 执行查询
- 自定义 symbol 参数过滤
"""

from __future__ import annotations

import pytest


class TestPnlToolModule:
    """验证模块的公开 API 表面"""

    def test_pnl_tool_exists(self):
        from axon_quant._native import trading

        assert hasattr(trading, "GetPnlTool")


class TestPnlToolConstruction:
    """GetPnlTool 构造行为"""

    def test_tool_construction(self):
        from axon_quant._native import trading

        backend = trading.MockTradingBackend()
        tool = trading.GetPnlTool(backend)
        assert tool.name == "get_pnl"
        assert "查询账户盈亏信息" in tool.description


class TestPnlToolExecution:
    """GetPnlTool 执行行为"""

    def test_default_args_returns_all_positions(self):
        from axon_quant._native import trading

        backend = trading.MockTradingBackend()
        tool = trading.GetPnlTool(backend)
        result = tool.execute({})

        assert "positions" in result
        assert "total_unrealized_pnl" in result
        assert "as_of_ms" in result

    def test_empty_args(self):
        from axon_quant._native import trading

        backend = trading.MockTradingBackend()
        tool = trading.GetPnlTool(backend)
        result = tool.execute()

        assert "positions" in result

    def test_symbol_filter(self):
        from axon_quant._native import trading

        backend = trading.MockTradingBackend()
        tool = trading.GetPnlTool(backend)
        result = tool.execute({"symbol": "BTC-USDT"})

        assert "positions" in result
        for pos in result["positions"]:
            assert pos["symbol"] == "BTC-USDT"

    def test_symbol_filter_non_existent(self):
        from axon_quant._native import trading

        backend = trading.MockTradingBackend()
        tool = trading.GetPnlTool(backend)
        result = tool.execute({"symbol": "ETH-USDT"})

        assert len(result["positions"]) == 0
        assert result["total_unrealized_pnl"] == 0.0

    def test_position_pnl_fields(self):
        from axon_quant._native import trading

        backend = trading.MockTradingBackend()
        tool = trading.GetPnlTool(backend)
        result = tool.execute({})

        for pos in result["positions"]:
            assert "symbol" in pos
            assert "quantity" in pos
            assert "entry_price" in pos
            assert "unrealized_pnl" in pos
            assert "unrealized_pnl_pct" in pos