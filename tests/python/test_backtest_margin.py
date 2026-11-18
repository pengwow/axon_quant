"""BacktestRunner 保证金绑定的行为验证（配置解析 + 结果字段透传）。"""

from axon_quant import worker


def _bars(start: float, step: float, n: int):
    return [
        {
            "timestamp": i * 60_000_000_000,
            "open": start + i * step - 0.5,
            "high": start + i * step + 0.3,
            "low": start + i * step - 0.8,
            "close": start + i * step,
            "volume": 1000.0,
        }
        for i in range(n)
    ]


class BuyStrategy:
    """鸭子类型策略：恒定买入，保证有持仓参与保证金检查。"""

    def __init__(self, symbol: str):
        self.symbol = symbol

    def on_bar(self, bar):
        return {"action_type": "buy", "confidence": 0.8, "target_position": 1.0}


# 最小档位配置：1 档、无上限，仅验证解析链路能跑通
BTC_BRACKETS = [
    {
        "notional_floor": 0.0,
        "notional_cap": 50_000.0,
        "mmr": 0.004,
        "max_leverage": 125.0,
    }
]


def test_with_margin_is_callable():
    """空 dict 等价于不启用，且 setter 返回 None（原地替换 runner 状态）。"""
    runner = worker.BacktestRunner(initial_cash=10_000.0, futures=True)
    assert runner.with_margin({}) is None


def test_margin_fields_default_when_disabled():
    """未启用保证金时结果字段保持中性默认值。"""
    runner = worker.BacktestRunner(initial_cash=10_000.0)
    runner.add_strategy(BuyStrategy("BTC/USDT"), symbol="BTC/USDT")

    result = runner.run(bars_by_symbol={"BTC/USDT": _bars(100.0, 1.0, 5)})

    assert result.liquidation_events == []
    assert result.insurance_fund_loss == 0.0
    assert result.price_basis == "close"
    assert result.margin_applied is False
    assert result.margin_specs_effective == []
    assert result.liquidation_count == 0
    assert result.shortfall_count == 0
    assert not hasattr(result, "capital_budget_disabled")


def test_with_margin_runs_with_minimal_brackets():
    """传入最小档位配置能正常跑完（不要求触发强平）。"""
    runner = worker.BacktestRunner(initial_cash=10_000.0, futures=True)
    runner.add_strategy(BuyStrategy("BTC/USDT"), symbol="BTC/USDT")
    runner.with_margin(
        {
            "BTC/USDT": {
                "leverage": 10.0,
                "mode": "cross",
                "brackets": BTC_BRACKETS,
            }
        }
    )

    result = runner.run(bars_by_symbol={"BTC/USDT": _bars(100.0, 1.0, 5)})

    assert result.n_bars == 5
    assert isinstance(result.liquidation_events, list)
    assert result.price_basis == "close"
    # 生效规格来自引擎实际构造的 tracker（档位来源缺省为 default）
    assert result.margin_applied is True
    assert result.margin_specs_effective == [
        {
            "symbol": "BTC/USDT",
            "leverage": 10.0,
            "margin_mode": "cross",
            "bracket_source": "default",
        }
    ]
    assert result.liquidation_count == len(result.liquidation_events)
    # 小额名义 100 USDT 远低于维持保证金阈值，不应产生强平
    assert result.liquidation_events == []


def test_result_exposes_effective_specs_and_counts():
    """未启用保证金时新增结果字段保持中性默认值，且旧 flag 已删除。"""
    runner = worker.BacktestRunner(initial_cash=10_000.0)
    runner.add_strategy(BuyStrategy("BTC/USDT"), symbol="BTC/USDT")

    result = runner.run(bars_by_symbol={"BTC/USDT": _bars(100.0, 1.0, 5)})

    assert result.margin_applied is False
    assert result.margin_specs_effective == []
    assert result.liquidation_count == 0
    assert result.shortfall_count == 0
    assert not hasattr(result, "capital_budget_disabled")
