"""Kernel.add_rl_strategy 绑定层行为验证（M4.5：RL 推理接入统一内核）。

用固定返回 0.5 的假 predict 跑 3 根 bar，断言：
1) predict 每 bar 被调用一次，观测长度 = features × window；
2) `SetTarget { 0.5 }` 真的成交（fills >= 1）；
3) 动作空间必须是能无损承载 [-1, 1] 的连续空间（恒等映射前提）。
"""

import pytest

from axon_quant import worker

FEATURES = [
    {
        "name": "close",
        "source": {"PriceField": "close"},
        "normalizer": "None",
        "clip_range": None,
    },
]

CONTINUOUS_1_1 = {"type": "continuous", "min": -1.0, "max": 1.0}


def _bars(n: int = 3, price: float = 100.0):
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


def test_rl_host_produces_fills():
    calls: list[int] = []

    def predict(obs):
        calls.append(len(obs))
        return 0.5

    kernel = worker.Kernel(initial_cash=10_000.0)
    kernel.add_rl_strategy(
        predict,
        "fake-model",
        "v1-test",
        1,
        FEATURES,
        CONTINUOUS_1_1,
        "BTC/USDT",
    )
    result = kernel.run(bars_by_symbol={"BTC/USDT": _bars()})

    assert calls == [1, 1, 1], "每 bar 一次推理，观测长度 = 1 特征 × 1 窗口"
    assert result.n_bars == 3
    assert result.fills >= 1, "0.5 权益占比必须真的成交"
    # 恒等映射校验：ratio 0.5 ⇒ qty = 0.5 × 10000 / 100（首根 bar 无滑点前的定价 100）
    assert result.fills_detail[0]["quantity"] == pytest.approx(50.0, rel=1e-6)


def test_rl_host_rejects_quantizing_action_space():
    kernel = worker.Kernel(initial_cash=10_000.0)
    with pytest.raises(ValueError, match="连续空间"):
        kernel.add_rl_strategy(
            lambda obs: 0.5,
            "fake-model",
            "v1-test",
            1,
            FEATURES,
            {"type": "discrete", "n_quantity_bins": 4},
            "BTC/USDT",
        )
