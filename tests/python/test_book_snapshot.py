"""GetBookSnapshotTool Python API 测试

覆盖:
- GetBookSnapshotTool 构造 + 默认配置
- get_book_snapshot 执行查询
- 自定义 levels 参数
"""

from __future__ import annotations

import json
import pytest


class TestBookSnapshotToolModule:
    """验证模块的公开 API 表面"""

    def test_book_snapshot_tool_exists(self):
        from axon_quant._native import trading

        assert hasattr(trading, "GetBookSnapshotTool")


class TestBookSnapshotToolConstruction:
    """GetBookSnapshotTool 构造行为"""

    def test_tool_construction(self):
        from axon_quant._native import trading
        from axon_quant.backtest import L1MatchingEngine

        engine = L1MatchingEngine()
        tool = trading.GetBookSnapshotTool(engine, "BTC-USDT")
        assert tool.name == "get_book_snapshot"
        assert "查询订单簿快照" in tool.description


class TestBookSnapshotToolExecution:
    """GetBookSnapshotTool 执行行为"""

    def test_empty_engine(self):
        from axon_quant._native import trading
        from axon_quant.backtest import L1MatchingEngine

        engine = L1MatchingEngine()
        tool = trading.GetBookSnapshotTool(engine, "BTC-USDT")
        data = tool.execute({})

        assert data["symbol"] == "BTC-USDT"
        assert len(data["bids"]) == 0
        assert len(data["asks"]) == 0

    def test_with_data(self):
        from axon_quant._native import trading
        from axon_quant.backtest import L1MatchingEngine, limit_order, spot_instrument

        engine = L1MatchingEngine()
        btc = spot_instrument("BTC", "USDT")
        engine.submit(limit_order(1, btc, "Sell", 101.0, 1.0))
        engine.submit(limit_order(2, btc, "Buy", 99.0, 1.0))

        tool = trading.GetBookSnapshotTool(engine, "BTC-USDT")
        data = tool.execute({})

        assert data["symbol"] == "BTC-USDT"
        assert len(data["bids"]) == 1
        assert len(data["asks"]) == 1

    def test_default_levels(self):
        from axon_quant._native import trading
        from axon_quant.backtest import L1MatchingEngine, limit_order, spot_instrument

        engine = L1MatchingEngine()
        btc = spot_instrument("BTC", "USDT")
        engine.submit(limit_order(1, btc, "Sell", 101.0, 1.0))
        engine.submit(limit_order(2, btc, "Sell", 102.0, 2.0))
        engine.submit(limit_order(3, btc, "Buy", 99.0, 1.0))
        engine.submit(limit_order(4, btc, "Buy", 98.0, 2.0))

        tool = trading.GetBookSnapshotTool(engine, "BTC-USDT")
        data = tool.execute({})

        assert len(data["bids"]) == 2
        assert len(data["asks"]) == 2

    def test_custom_levels(self):
        from axon_quant._native import trading
        from axon_quant.backtest import L1MatchingEngine, limit_order, spot_instrument

        engine = L1MatchingEngine()
        btc = spot_instrument("BTC", "USDT")
        engine.submit(limit_order(1, btc, "Sell", 101.0, 1.0))
        engine.submit(limit_order(2, btc, "Sell", 102.0, 2.0))
        engine.submit(limit_order(3, btc, "Buy", 99.0, 1.0))
        engine.submit(limit_order(4, btc, "Buy", 98.0, 2.0))

        tool = trading.GetBookSnapshotTool(engine, "BTC-USDT")
        data = tool.execute({"levels": 1})

        assert len(data["bids"]) == 1
        assert len(data["asks"]) == 1