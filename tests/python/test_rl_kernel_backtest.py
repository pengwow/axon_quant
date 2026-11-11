"""rl.kernel_backtest 设施验证 — 假 predict_fn 跑统一内核，断言 15 字段响应契约。

参照 test_kernel_rl.py 的构造惯例（_bars / FEATURES / 恒定 0.5 action）。
"""

import pytest

from axon_quant.rl.kernel_backtest import run_kernel_backtest

FEATURES = [
    {
        "name": "close",
        "source": {"PriceField": "close"},
        "normalizer": "None",
        "clip_range": None,
    },
]
CONTINUOUS_1_1 = {"type": "continuous", "min": -1.0, "max": 1.0}
SPEC = {
    "spec_version": "v1-test",
    "window_size": 1,
    "features": FEATURES,
    "action_space": CONTINUOUS_1_1,
}


def _bars(n=3, price=100.0):
    return [
        {
            "timestamp": i * 60_000_000_000,
            "open": price,
            "high": price + 1.0,
            "low": price - 1.0,
            "close": price,
            "volume": 1_000.0,
        }
        for i in range(n)
    ]


RESPONSE_KEYS = {
    "symbol", "interval", "data_bars", "initial_capital", "total_pnl",
    "total_return_pct", "sharpe_ratio", "max_drawdown_pct", "win_rate",
    "num_trades", "profit_factor", "total_fees", "total_funding",
    "turnover", "engine",
}


def test_run_kernel_backtest_response_contract():
    raw = run_kernel_backtest(
        bars=_bars(),
        predict_fn=lambda obs: 0.5,
        symbol="BTC/USDT",
        interval="1h",
        spec=SPEC,
        initial_capital=10_000.0,
        fee_rate=0.0,
        model_id="fake-model",
    )
    assert set(raw) == RESPONSE_KEYS
    assert raw["engine"] == "kernel"
    assert raw["symbol"] == "BTC/USDT"          # 设施回显入参（归一化 symbol）
    assert raw["interval"] == "1h"
    assert raw["data_bars"] == 3
    assert raw["initial_capital"] == 10_000.0
    assert raw["num_trades"] >= 1               # 0.5 权益占比必须真的成交
    assert raw["profit_factor"] is None         # 内核不提供 round-trip 盈亏比
    assert raw["total_fees"] == 0.0             # fee_rate=0
    # 撮合价由内核决定（不假设理想 close 成交），只验契约：Σ|price×qty| 存在且为正
    assert raw["turnover"] is not None and raw["turnover"] > 0
    assert isinstance(raw["total_pnl"], float)
    assert isinstance(raw["sharpe_ratio"], float)
    assert isinstance(raw["max_drawdown_pct"], float)


def test_run_kernel_backtest_respects_fee_rate():
    raw = run_kernel_backtest(
        bars=_bars(),
        predict_fn=lambda obs: 0.5,
        symbol="BTC/USDT",
        interval="1h",
        spec=SPEC,
        initial_capital=10_000.0,
        fee_rate=0.0001,                        # pass-through，不内置应用默认
        model_id="fake-model",
    )
    assert raw["total_fees"] > 0.0
    # 费率口径配对（原实现注释）：total_fees = Σ|price×qty| × fee_rate
    assert raw["turnover"]
    assert raw["total_fees"] == pytest.approx(raw["turnover"] * 0.0001, rel=1e-6)
