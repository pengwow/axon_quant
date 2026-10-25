"""PaperDriver 绑定层冒烟验证（Task 11.4：RL paper 循环）。

覆盖不联网即可验证的完整边界：构造校验（空品种 / 非法 initial_cash /
fee_rate）、RL 策略注册、无宿主 / 未 start 的 fail-fast，以及 feed_bar
全链路的闭式账本断言（建仓 → 盯市 → 清仓）。paper 账纯内存、无外部
连接，测试直接灌收市 bar 驱动。

成交价口径：PyPaperDriver 未暴露 seed 参数，PaperSink 用 SeedLiquidity
默认 half_spread=0.0005（绝对价差）种盘口——买单吃 ask=mid+0.0005、
卖单吃 bid=mid−0.0005；SetTarget 换算 qty = ratio × equity / close。
"""

import pytest

from axon_quant import worker

START = 1_700_000_000_000_000_000
ONE_MIN = 60_000_000_000

CONTINUOUS_1_1 = {"type": "continuous", "min": -1.0, "max": 1.0}

FEATURES = [
    {
        "name": "close",
        "source": {"PriceField": "close"},
        "normalizer": "None",
        "clip_range": None,
    },
]


def test_constructor_accepts_defaults():
    d = worker.PaperDriver(["BTCUSDT"])
    assert type(d).__name__ == "PaperDriver"


def test_constructor_rejects_empty_symbols():
    with pytest.raises(ValueError, match="至少需要一个交易品种"):
        worker.PaperDriver([])


def test_constructor_rejects_negative_cash():
    with pytest.raises(ValueError, match="initial_cash"):
        worker.PaperDriver(["BTCUSDT"], initial_cash=-1.0)


def test_constructor_rejects_negative_fee_rate():
    with pytest.raises(ValueError, match="fee_rate"):
        worker.PaperDriver(["BTCUSDT"], fee_rate=-0.001)


def test_add_rl_strategy_registers():
    d = worker.PaperDriver(["BTCUSDT"])
    d.add_rl_strategy(
        lambda obs: 0.0, "m1", "v1-rl", 1, FEATURES, CONTINUOUS_1_1, "BTC/USDT"
    )


def test_add_rl_strategy_rejects_non_callable():
    d = worker.PaperDriver(["BTCUSDT"])
    with pytest.raises(TypeError, match="predict"):
        d.add_rl_strategy(0.5, "m1", "v1-rl", 1, FEATURES, CONTINUOUS_1_1, "BTC/USDT")


def test_start_without_host_fails_fast():
    d = worker.PaperDriver(["BTCUSDT"])
    with pytest.raises((ValueError, RuntimeError), match="至少需要一个策略宿主"):
        d.start()


def test_feed_bar_before_start_fails():
    d = worker.PaperDriver(["BTCUSDT"])
    with pytest.raises(RuntimeError, match="内核未启动"):
        d.feed_bar("BTCUSDT", START, 100.0, 100.0, 100.0, 100.0, 10.0)


def test_shutdown_is_idempotent_noop():
    d = worker.PaperDriver(["BTCUSDT"])
    d.shutdown()
    d.shutdown()


def test_rl_feed_bar_roundtrip_matches_paper_ledger():
    """RL 策略灌两根 bar：建仓 0.3 → 清仓 0.0，逐项闭式核对资金链（fee=0）。

    bar0 close=100：qty = 0.3 × 10000 / 100 = 30，买单价 = 100.0005，
    cash = 10000 − 30×100.0005 = 6999.985，盯市 equity = 6999.985 + 3000；
    bar1 close=110：ratio=0 清仓，卖单价 = 109.9995，
    cash = 6999.985 + 30×109.9995 = 10299.97，持仓清空。
    """
    d = worker.PaperDriver(["BTCUSDT"], initial_cash=10_000.0, fee_rate=0.0)
    ratios = iter([0.3, 0.0])
    d.add_rl_strategy(
        lambda obs: next(ratios), "m1", "v1-rl", 1, FEATURES, CONTINUOUS_1_1, "BTC/USDT"
    )
    d.start()

    # bar0 建仓
    d.feed_bar("BTCUSDT", START, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    d.tick()
    snap = d.snapshot()
    assert snap["positions"]["BTC/USDT"] == pytest.approx(30.0, abs=1e-4)
    assert snap["equity"] == pytest.approx(9_999.985, abs=1e-4)

    # bar1 清仓
    d.feed_bar("BTCUSDT", START + ONE_MIN, 110.0, 111.0, 109.0, 110.0, 10_000.0)
    d.tick()
    snap = d.snapshot()
    assert snap["positions"] == {}
    assert snap["equity"] == pytest.approx(10_299.97, abs=1e-4)
