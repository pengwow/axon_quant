"""BacktestTradingBackend Python 端测试

覆盖:
- BacktestTradingBackend 导入 + 构造
- place_order 下单
- query_portfolio 查询投资组合
- book_snapshot 获取订单簿快照
- advance_bar 推进 bar
"""

from __future__ import annotations


class TestBacktestTradingBackend:
    """BacktestTradingBackend Python 绑定测试"""

    def test_backend_importable(self):
        from axon_quant.trading import BacktestTradingBackend

        assert BacktestTradingBackend is not None

    def test_backend_default_construction(self):
        from axon_quant.trading import BacktestTradingBackend

        backend = BacktestTradingBackend()
        assert backend is not None
        assert "BacktestTradingBackend" in repr(backend)

    def test_backend_custom_symbol_and_cash(self):
        from axon_quant.trading import BacktestTradingBackend

        backend = BacktestTradingBackend(symbol="ETH-USDT", initial_cash=50000.0)
        assert backend is not None

    def test_place_order_minimal(self):
        from axon_quant.trading import BacktestTradingBackend

        backend = BacktestTradingBackend()
        ack = backend.place_order({
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.1,
            "price": 50000.0,
        })
        assert ack is not None
        assert "order_id" in ack
        assert "symbol" in ack
        assert "side" in ack
        assert "status" in ack

    def test_place_order_missing_price_for_limit(self):
        from axon_quant.trading import BacktestTradingBackend

        backend = BacktestTradingBackend()
        try:
            backend.place_order({
                "symbol": "BTC-USDT",
                "side": "Buy",
                "quantity": 0.1,
            })
            assert False, "should raise error"
        except Exception:
            pass

    def test_query_portfolio_returns_balance_and_positions(self):
        from axon_quant.trading import BacktestTradingBackend

        backend = BacktestTradingBackend()
        portfolio = backend.query_portfolio()
        assert "balance" in portfolio
        assert "positions" in portfolio
        assert isinstance(portfolio["positions"], list)

    def test_book_snapshot_default_levels(self):
        from axon_quant.trading import BacktestTradingBackend

        backend = BacktestTradingBackend()
        snapshot = backend.book_snapshot()
        assert "symbol" in snapshot
        assert "bids" in snapshot
        assert "asks" in snapshot
        assert isinstance(snapshot["bids"], list)
        assert isinstance(snapshot["asks"], list)

    def test_book_snapshot_custom_levels(self):
        from axon_quant.trading import BacktestTradingBackend

        backend = BacktestTradingBackend()
        snapshot = backend.book_snapshot(levels=5)
        assert "symbol" in snapshot
        assert "bids" in snapshot
        assert "asks" in snapshot

    def test_book_snapshot_uses_backend_symbol(self):
        """验证 book_snapshot 使用后端实际的 symbol，而非硬编码"""
        from axon_quant.trading import BacktestTradingBackend

        backend = BacktestTradingBackend(symbol="ETH-USDT")
        snapshot = backend.book_snapshot()
        assert snapshot["symbol"] == "ETH-USDT", (
            f"symbol 应该是 'ETH-USDT'，实际是 '{snapshot['symbol']}'"
        )

    def test_advance_bar_injects_liquidity(self):
        """验证 advance_bar 后订单簿有实际数据"""
        from axon_quant.trading import BacktestTradingBackend

        backend = BacktestTradingBackend(symbol="BTC-USDT")
        # advance_bar 前订单簿为空
        before = backend.book_snapshot()
        assert len(before["bids"]) == 0
        assert len(before["asks"]) == 0

        # 推进 bar，注入流动性
        backend.advance_bar(
            mid_price=50000.0,
            half_spread=50.0,
            depth_levels=5,
            size_per_level=1.0,
        )

        # advance_bar 后订单簿有数据
        after = backend.book_snapshot()
        assert len(after["bids"]) == 5
        assert len(after["asks"]) == 5
        # 卖一盘应该等于 mid_price + half_spread
        assert after["asks"][0]["price"] == 50050.0
        # 买一盘应该等于 mid_price - half_spread
        assert after["bids"][0]["price"] == 49950.0

    def test_advance_bar_returns_status(self):
        from axon_quant.trading import BacktestTradingBackend

        backend = BacktestTradingBackend()
        result = backend.advance_bar(
            mid_price=50000.0,
            half_spread=50.0,
            depth_levels=10,
            size_per_level=1.0,
        )
        assert "status" in result
        assert result["status"] == "ok"
        assert "mid_price" in result
        assert result["mid_price"] == 50000.0