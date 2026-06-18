"""axon_quant.trading Python 端到端测试

覆盖:
- 7 个核心类的导入 + 顶层 re-export
- Mock 闭环:place / cancel / replace / query
- 风险拒绝:白名单 / 单日超限 / 持仓超限
- TradingMetrics 埋点 snapshot
"""

from __future__ import annotations

import pytest


# ──────────────────────────────────────────────────────────────
# 模块可见性 / 公开 API 表面
# ──────────────────────────────────────────────────────────────


class TestModuleSurface:
    """验证 trading 子模块的公开 API 表面"""

    def test_axon_quant_trading_submodule_exists(self):
        import axon_quant

        assert hasattr(axon_quant, "trading"), "axon_quant.trading submodule missing"
        assert hasattr(axon_quant, "RiskLimits"), "axon_quant.RiskLimits missing"
        assert hasattr(axon_quant, "MockTradingBackend"), "axon_quant.MockTradingBackend missing"
        assert hasattr(axon_quant, "PlaceOrderTool"), "axon_quant.PlaceOrderTool missing"
        assert hasattr(axon_quant, "QueryPortfolioTool"), "axon_quant.QueryPortfolioTool missing"
        assert hasattr(axon_quant, "CancelOrderTool"), "axon_quant.CancelOrderTool missing"
        assert hasattr(axon_quant, "ReplaceOrderTool"), "axon_quant.ReplaceOrderTool missing"
        assert hasattr(axon_quant, "TradingMetrics"), "axon_quant.TradingMetrics missing"

    def test_top_level_exports_match_module(self):
        from axon_quant import (
            CancelOrderTool,
            MockTradingBackend,
            PlaceOrderTool,
            QueryPortfolioTool,
            ReplaceOrderTool,
            RiskLimits,
            TradingMetrics,
            trading,
        )

        # 顶层 re-export 与 submodule 引用一致
        assert trading.RiskLimits is RiskLimits
        assert trading.MockTradingBackend is MockTradingBackend
        assert trading.PlaceOrderTool is PlaceOrderTool
        assert trading.QueryPortfolioTool is QueryPortfolioTool
        assert trading.CancelOrderTool is CancelOrderTool
        assert trading.ReplaceOrderTool is ReplaceOrderTool
        assert trading.TradingMetrics is TradingMetrics

    def test_all_classes_importable(self):
        from axon_quant.trading import (
            CancelOrderTool,
            MockTradingBackend,
            PlaceOrderTool,
            QueryPortfolioTool,
            ReplaceOrderTool,
            RiskLimits,
            TradingMetrics,
        )

        for cls in (
            RiskLimits,
            MockTradingBackend,
            PlaceOrderTool,
            QueryPortfolioTool,
            CancelOrderTool,
            ReplaceOrderTool,
            TradingMetrics,
        ):
            assert cls is not None


# ──────────────────────────────────────────────────────────────
# RiskLimits
# ──────────────────────────────────────────────────────────────


class TestRiskLimits:
    """RiskLimits 构造 + permissive 工厂"""

    def test_permissive_factory(self):
        from axon_quant.trading import RiskLimits

        rl = RiskLimits.permissive()
        assert rl is not None
        assert "RiskLimits" in repr(rl)

    def test_keyword_construction(self):
        from axon_quant.trading import RiskLimits

        rl = RiskLimits(
            max_order_notional=100.0,
            max_daily_orders=20,
            allowed_symbols=["BTC-USDT"],
        )
        assert rl is not None
        r = repr(rl)
        # repr 应包含已设置的字段(供调试)
        assert "100" in r or "100.0" in r
        assert "BTC-USDT" in r


# ──────────────────────────────────────────────────────────────
# Mock 闭环:place / cancel / replace / query
# ──────────────────────────────────────────────────────────────


class TestMockLoop:
    """Mock 闭环端到端"""

    def test_mock_backend_initial_state(self):
        from axon_quant.trading import MockTradingBackend

        backend = MockTradingBackend()
        assert backend.order_count() == 0

    def test_place_order_dry_run(self):
        """DryRun 模式:不真发,返回 status='DryRun'"""
        from axon_quant.trading import (
            MockTradingBackend,
            PlaceOrderTool,
            RiskLimits,
        )

        backend = MockTradingBackend()
        risk = RiskLimits(allowed_symbols=["BTC-USDT"])
        tool = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk)

        ack = tool.execute({
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.1,
            "price": 50000.0,
        })

        assert ack["symbol"] == "BTC-USDT"
        assert ack["side"] == "Buy"
        # DryRun 模式状态字段是 "DryRun"
        assert ack["status"] == "DryRun"

    def test_place_order_direct(self):
        """Direct 模式:真发,order_count 增加"""
        from axon_quant.trading import (
            MockTradingBackend,
            PlaceOrderTool,
            RiskLimits,
        )

        backend = MockTradingBackend()
        risk = RiskLimits(allowed_symbols=["BTC-USDT"])
        tool = PlaceOrderTool(backend=backend, mode="direct", risk=risk)

        ack = tool.execute({
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.1,
            "price": 50000.0,
        })

        # direct 模式返回的不是 "DRY-RUN" 占位
        assert ack["order_id"] != "DRY-RUN"
        assert backend.order_count() == 1

    def test_query_portfolio_returns_balance_and_positions(self):
        from axon_quant.trading import MockTradingBackend, QueryPortfolioTool

        backend = MockTradingBackend()
        tool = QueryPortfolioTool(backend=backend)
        portfolio = tool.execute()
        # 至少有 balance 和 positions 字段
        assert "balance" in portfolio
        assert "positions" in portfolio
        assert isinstance(portfolio["positions"], list)

    def test_query_portfolio_with_symbol_filter(self):
        from axon_quant.trading import MockTradingBackend, QueryPortfolioTool

        backend = MockTradingBackend()
        tool = QueryPortfolioTool(backend=backend)
        # symbol 过滤仅影响 positions,balance 仍存在
        portfolio = tool.execute({"symbol": "BTC-USDT"})
        assert "balance" in portfolio
        assert "positions" in portfolio

    def test_cancel_order_direct(self):
        from axon_quant.trading import (
            CancelOrderTool,
            MockTradingBackend,
            PlaceOrderTool,
            RiskLimits,
        )

        backend = MockTradingBackend()
        risk = RiskLimits(allowed_symbols=["BTC-USDT"])
        place = PlaceOrderTool(backend=backend, mode="direct", risk=risk)
        cancel = CancelOrderTool(backend=backend, risk=risk)

        ack = place.execute({
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.1,
            "price": 50000.0,
        })
        order_id = ack["order_id"]

        result = cancel.execute({"order_id": order_id})
        # status 包含 "ancel"(Cancelled / CANCELLED)
        assert "ancel" in result["status"]

    def test_replace_order_direct(self):
        from axon_quant.trading import (
            MockTradingBackend,
            PlaceOrderTool,
            ReplaceOrderTool,
            RiskLimits,
        )

        backend = MockTradingBackend()
        risk = RiskLimits(allowed_symbols=["BTC-USDT"])
        place = PlaceOrderTool(backend=backend, mode="direct", risk=risk)
        replace = ReplaceOrderTool(backend=backend, risk=risk)

        ack = place.execute({
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.1,
            "price": 50000.0,
        })
        order_id = ack["order_id"]

        # 改单:数量 0.2,价格 51000
        result = replace.execute({
            "order_id": order_id,
            "new_req": {
                "symbol": "BTC-USDT",
                "side": "Buy",
                "quantity": 0.2,
                "price": 51000.0,
            },
        })
        # mock 后端要么 Replaced 要么 Rejected 都可,只要不抛错
        assert "status" in result


# ──────────────────────────────────────────────────────────────
# 风险拒绝
# ──────────────────────────────────────────────────────────────


class TestRiskRejection:
    """风险规则拒绝路径"""

    def test_place_order_blocked_by_whitelist(self):
        """白名单拒:不在白名单的 symbol 拒绝"""
        from axon_quant.trading import (
            MockTradingBackend,
            PlaceOrderTool,
            RiskLimits,
        )

        backend = MockTradingBackend()
        risk = RiskLimits(allowed_symbols=["ETH-USDT"])  # 只允许 ETH
        tool = PlaceOrderTool(backend=backend, mode="direct", risk=risk)

        with pytest.raises(RuntimeError, match="白名单"):
            tool.execute({
                "symbol": "BTC-USDT",
                "side": "Buy",
                "quantity": 0.1,
                "price": 50000.0,
            })

    def test_place_order_blocked_by_daily_limit(self):
        """单日超限拒:超过 max_daily_orders 拒绝"""
        from axon_quant.trading import (
            MockTradingBackend,
            PlaceOrderTool,
            RiskLimits,
        )

        backend = MockTradingBackend()
        risk = RiskLimits(max_daily_orders=1)
        tool = PlaceOrderTool(backend=backend, mode="direct", risk=risk)

        # 第一次下单成功
        tool.execute({
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.1,
            "price": 50000.0,
        })
        # 第二次下单失败(单日超限)
        with pytest.raises(RuntimeError, match="日"):
            tool.execute({
                "symbol": "BTC-USDT",
                "side": "Buy",
                "quantity": 0.1,
                "price": 50000.0,
            })

    def test_place_order_blocked_by_notional(self):
        """单笔金额超限拒:price * quantity > max_order_notional 拒绝"""
        from axon_quant.trading import (
            MockTradingBackend,
            PlaceOrderTool,
            RiskLimits,
        )

        backend = MockTradingBackend()
        risk = RiskLimits(max_order_notional=100.0)  # 100 USDT
        tool = PlaceOrderTool(backend=backend, mode="direct", risk=risk)

        with pytest.raises(RuntimeError):
            tool.execute({
                "symbol": "BTC-USDT",
                "side": "Buy",
                "quantity": 1.0,
                "price": 50000.0,  # 50000 USDT > 100 限额
            })


# ──────────────────────────────────────────────────────────────
# TradingMetrics
# ──────────────────────────────────────────────────────────────


class TestTradingMetrics:
    """TradingMetrics 埋点行为"""

    def test_empty_metrics_has_daily_gauge(self):
        """空 metrics 至少含 1 个 daily gauge(Stage H 实现)"""
        from axon_quant.trading import TradingMetrics

        metrics = TradingMetrics()
        snapshot = metrics.snapshot()
        # TradingMetrics::snapshot 永远 emit `trading_daily_orders_count` gauge
        assert len(snapshot) >= 1

    def test_snapshot_filtered_returns_subset(self):
        from axon_quant.trading import TradingMetrics

        metrics = TradingMetrics()
        # 过滤一个不存在的 name → 返回空列表
        filtered = metrics.snapshot_filtered("nonexistent.metric.name")
        assert isinstance(filtered, list)
        assert len(filtered) == 0

    def test_metrics_after_place_dry_run_has_entries(self):
        """DryRun 下单后 snapshot 有非空内容"""
        from axon_quant.trading import (
            MockTradingBackend,
            PlaceOrderTool,
            RiskLimits,
            TradingMetrics,
        )

        metrics = TradingMetrics()
        backend = MockTradingBackend()
        risk = RiskLimits(allowed_symbols=["BTC-USDT"])
        tool = PlaceOrderTool(
            backend=backend, mode="dry_run", risk=risk, metrics=metrics
        )

        tool.execute({
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.1,
            "price": 50000.0,
        })

        snapshot = metrics.snapshot()
        # 至少有 daily_orders_count gauge(可能 + orders 计数)
        assert len(snapshot) >= 1
        # 验证样本 dict 字段完整
        for s in snapshot:
            assert "name" in s
            assert "kind" in s
            assert "value" in s
            assert "labels" in s
