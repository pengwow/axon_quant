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


# 训练侧导出 → 内核灌回：close 走 Z-Score，喂了统计量才不是恒 0
ZSCORE_FEATURES = [
    {
        "name": "close",
        "source": {"PriceField": "close"},
        "normalizer": "ZScore",
        "clip_range": None,
    },
    {
        "name": "volume",
        "source": {"VolumeField": "volume"},
        "normalizer": "None",
        "clip_range": None,
    },
]


def test_observation_stats_round_trip():
    """`TradingEnv.observation_stats()` 的 JSON 能被内核 `stats=` 原样吃回。

    断言两件事：
    1) 灌回统计量后，内核观测里的 close 维度是训练口径的 Z-Score（不再是恒 0）；
    2) stats 条数与特征数不符时注册阶段就报错，而不是推理时越界。
    """
    import json

    from axon_quant import rl

    env = rl.TradingEnv(
        config={"initial_capital": 10_000.0, "symbol": "BTCUSDT"},
        market_data=_bars(),
    )
    stats = json.loads(env.observation_stats())
    assert len(stats) == 2, "默认观测空间 close + volume 各一条统计"
    # 模拟训练期积累：close 在 [80, 100] 上 mean=90、样本标准差=10
    stats[0] = {
        "mean": 90.0,
        "m2": 200.0,
        "min": 80.0,
        "max": 100.0,
        "count": 3,
        "median_buffer": [80.0, 90.0, 100.0],
    }

    # 绕一圈：导出 → 灌回，训练 env 与内核吃的是同一份 JSON 文本
    stats_json = json.dumps(stats)
    env.set_observation_stats(stats_json)
    assert env.reset()["features"][0] == pytest.approx(1.0, rel=1e-9)

    seen: list[list[float]] = []

    def predict(obs):
        seen.append(list(obs))
        return 0.0

    kernel = worker.Kernel(initial_cash=10_000.0)
    kernel.add_rl_strategy(
        predict,
        "fake-model",
        "v1-test",
        1,
        ZSCORE_FEATURES,
        CONTINUOUS_1_1,
        "BTC/USDT",
        stats,
    )
    kernel.run(bars_by_symbol={"BTC/USDT": _bars()})

    assert seen, "predict 必须被调用"
    assert seen[0][0] == pytest.approx(1.0, rel=1e-9), "close 必须按训练统计量归一化"
    assert seen[0][1] == pytest.approx(1_000.0), "volume 走 None 归一化，原样透传"

    with pytest.raises(ValueError, match="stats 条数"):
        kernel.add_rl_strategy(
            predict,
            "fake-model",
            "v1-test",
            1,
            ZSCORE_FEATURES,
            CONTINUOUS_1_1,
            "BTC/USDT",
            stats[:1],
        )
