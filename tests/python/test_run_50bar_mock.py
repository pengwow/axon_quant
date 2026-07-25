"""run_50bar.py 测试

覆盖:
- 合成数据生成
- MockProvider 响应生成
- TradingLoop 运行
- Trajectory 落盘
"""

from __future__ import annotations

import json
import os
import sys
import tempfile

import pytest

# 添加 examples 目录到 Python 路径
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))


class TestGenerateSyntheticData:
    """合成数据生成测试"""

    def test_generates_correct_count(self):
        from examples.llm_trading.run_50bar import generate_synthetic_data

        bars = generate_synthetic_data(42, count=50)
        assert len(bars) == 50

    def test_bars_have_correct_fields(self):
        from examples.llm_trading.run_50bar import generate_synthetic_data, BarData

        bars = generate_synthetic_data(42, count=5)
        for bar in bars:
            assert isinstance(bar, BarData)
            assert bar.bar_id >= 0
            assert bar.ts > 0
            assert bar.open > 0
            assert bar.high >= bar.open
            assert bar.low <= bar.open
            assert bar.close > 0
            assert bar.volume > 0

    def test_deterministic_same_seed(self):
        from examples.llm_trading.run_50bar import generate_synthetic_data

        bars1 = generate_synthetic_data(42)
        bars2 = generate_synthetic_data(42)
        assert len(bars1) == len(bars2)
        for b1, b2 in zip(bars1, bars2):
            assert b1.close == b2.close


class TestMockProvider:
    """MockProvider 测试"""

    def test_provider_constructs(self):
        from examples.llm_trading.run_50bar import MockProvider

        provider = MockProvider(42)
        assert provider is not None

    def test_response_has_thought_and_action(self):
        from examples.llm_trading.run_50bar import MockProvider

        provider = MockProvider(42)
        response = provider.generate_response("test prompt")
        assert "thought" in response
        assert "action" in response

    def test_action_is_none_or_place_order(self):
        from examples.llm_trading.run_50bar import MockProvider

        provider = MockProvider(42)
        for _ in range(20):
            response = provider.generate_response("")
            action = response["action"]
            if action is not None:
                assert action["tool"] == "place_order"
                assert "args" in action
                assert action["args"]["symbol"] == "BTC-USDT"
                assert action["args"]["side"] in ["Buy", "Sell"]


class TestTradingLoop:
    """TradingLoop 测试"""

    def test_loop_constructs(self):
        from examples.llm_trading.run_50bar import TradingLoop

        loop = TradingLoop(seed=42)
        assert loop is not None
        assert loop.seed == 42
        assert loop.cash == 100000.0
        assert loop.position == 0.0

    def test_run_generates_50_bars(self):
        from examples.llm_trading.run_50bar import TradingLoop

        with tempfile.TemporaryDirectory() as tmpdir:
            loop = TradingLoop(seed=42, output_dir=tmpdir)
            trajectory = loop.run()
            assert len(trajectory["bars"]) == 50

    def test_trajectory_has_correct_structure(self):
        from examples.llm_trading.run_50bar import TradingLoop

        with tempfile.TemporaryDirectory() as tmpdir:
            loop = TradingLoop(seed=42, output_dir=tmpdir)
            trajectory = loop.run()

            assert trajectory["version"] == "0.10.0"
            assert trajectory["run_id"] == "run-42"
            assert trajectory["instrument"] == "BTC-USDT"
            assert trajectory["provider"] == "mock"
            assert trajectory["model"] == "mock-model"
            assert trajectory["seed"] == 42

            for bar in trajectory["bars"]:
                assert "bar_id" in bar
                assert "ts" in bar
                assert "thought" in bar
                assert "action" in bar
                assert "reward" in bar
                assert "cum_pnl" in bar

            assert trajectory["summary"] is not None
            assert "total_pnl" in trajectory["summary"]
            assert "trades" in trajectory["summary"]
            assert "final_position" in trajectory["summary"]

    def test_trajectory_file_written(self):
        from examples.llm_trading.run_50bar import TradingLoop

        with tempfile.TemporaryDirectory() as tmpdir:
            loop = TradingLoop(seed=42, output_dir=tmpdir)
            loop.run()

            expected_path = os.path.join(tmpdir, "trajectory_42.json")
            assert os.path.exists(expected_path)

            with open(expected_path) as f:
                loaded = json.load(f)
                assert loaded["run_id"] == "run-42"
                assert len(loaded["bars"]) == 50

    def test_cash_and_position_update_on_trade(self):
        from examples.llm_trading.run_50bar import TradingLoop

        with tempfile.TemporaryDirectory() as tmpdir:
            loop = TradingLoop(seed=12345, output_dir=tmpdir)
            initial_cash = loop.cash
            initial_position = loop.position

            trajectory = loop.run()

            assert loop.trade_count >= 0
            assert loop.cash != initial_cash or loop.position != initial_position


class TestRuleBasedMockProvider:
    """RuleBasedMockProvider 测试"""

    def test_provider_constructs(self):
        from examples.llm_trading.mock_provider import RuleBasedMockProvider

        provider = RuleBasedMockProvider(42)
        assert provider is not None
        assert provider.sma_short == 5
        assert provider.sma_long == 20
        assert len(provider.price_history) == 0

    def test_update_price(self):
        from examples.llm_trading.mock_provider import RuleBasedMockProvider

        provider = RuleBasedMockProvider(42)
        provider.update_price(50000.0)
        provider.update_price(50100.0)
        assert len(provider.price_history) == 2
        assert provider.price_history[-1] == 50100.0

    def test_response_has_thought_and_action(self):
        from examples.llm_trading.mock_provider import RuleBasedMockProvider

        provider = RuleBasedMockProvider(42)
        response = provider.generate_response("test prompt")
        assert "thought" in response
        assert "action" in response

    def test_sma_calculation(self):
        from examples.llm_trading.mock_provider import RuleBasedMockProvider

        provider = RuleBasedMockProvider(42)
        for i in range(10):
            provider.update_price(50000.0 + i * 100)

        sma5 = provider._calculate_sma(provider.price_history, 5)
        expected_sma5 = sum([50500, 50600, 50700, 50800, 50900]) / 5
        assert sma5 == expected_sma5

    def test_rsi_calculation(self):
        from examples.llm_trading.mock_provider import RuleBasedMockProvider

        provider = RuleBasedMockProvider(42)
        for i in range(20):
            provider.update_price(50000.0 + i * 50)

        rsi = provider._calculate_rsi(provider.price_history)
        assert 0 <= rsi <= 100

    def test_sma_crossover_buy_signal(self):
        from examples.llm_trading.mock_provider import RuleBasedMockProvider

        provider = RuleBasedMockProvider(42)
        for i in range(25):
            provider.update_price(50000.0 + i * 20)

        for _ in range(10):
            response = provider.generate_response("")
            if response["action"] is not None:
                assert response["action"]["tool"] == "place_order"
                break

    def test_action_is_none_or_place_order(self):
        from examples.llm_trading.mock_provider import RuleBasedMockProvider

        provider = RuleBasedMockProvider(42)
        for i in range(30):
            provider.update_price(50000.0 + i * 10)

        for _ in range(20):
            response = provider.generate_response("")
            action = response["action"]
            if action is not None:
                assert action["tool"] == "place_order"
                assert "args" in action
                assert action["args"]["symbol"] == "BTC-USDT"
                assert action["args"]["side"] in ["Buy", "Sell"]


class TestTrajectorySchema:
    """Trajectory Schema 验证测试"""

    def test_schema_loads(self):
        import json
        import os

        schema_path = os.path.join(
            os.path.dirname(__file__),
            "..",
            "..",
            "examples",
            "llm_trading",
            "trajectory.schema.json",
        )
        assert os.path.exists(schema_path)

        with open(schema_path) as f:
            schema = json.load(f)
            assert "title" in schema
            assert schema["title"] == "Trading Trajectory"

    def test_trajectory_validates_against_schema(self):
        import json
        import os
        import tempfile

        from examples.llm_trading.run_50bar import TradingLoop

        schema_path = os.path.join(
            os.path.dirname(__file__),
            "..",
            "..",
            "examples",
            "llm_trading",
            "trajectory.schema.json",
        )
        with open(schema_path) as f:
            schema = json.load(f)

        with tempfile.TemporaryDirectory() as tmpdir:
            loop = TradingLoop(seed=42, output_dir=tmpdir)
            trajectory = loop.run()

            _validate_json(trajectory, schema)

    def test_invalid_trajectory_fails_validation(self):
        import json
        import os

        schema_path = os.path.join(
            os.path.dirname(__file__),
            "..",
            "..",
            "examples",
            "llm_trading",
            "trajectory.schema.json",
        )
        with open(schema_path) as f:
            schema = json.load(f)

        invalid_trajectory = {
            "version": "0.10.0",
            "run_id": "run-42",
            "instrument": "BTC-USDT",
            "provider": "mock",
            "model": "mock-model",
            "seed": "invalid",
            "bars": [],
        }

        with pytest.raises(Exception):
            _validate_json(invalid_trajectory, schema)


def _validate_json(data: dict, schema: dict):
    from jsonschema import validate, ValidationError

    try:
        validate(instance=data, schema=schema)
    except ValidationError as e:
        raise ValueError(f"JSON validation failed: {e.message}") from e