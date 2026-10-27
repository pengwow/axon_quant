"""LiveDriver 绑定层冒烟验证（M4.6：实时驱动面）。

覆盖不联网即可验证的完整边界：v1 口径拒绝（现货 / 空品种 / cash 记账）、
RL 策略注册、无宿主 start 的 fail-fast。构造链 `ExchangeConfig → LiveDriver`
真实通过 BinanceAdapter（构造期不发网络请求），connect / tick 需要真实
交易所连接，由 testnet 演练（Task 12.3）覆盖。
"""

import pytest

from axon_quant import exchange, worker

BASE_URL = "https://testnet.binancefuture.com"
WS_URL = "wss://stream.testnet.binancefuture.com"


def _config():
    return exchange.ExchangeConfig(
        exchange.ExchangeId.Binance, "k", "s", BASE_URL, WS_URL, True
    )


CONTINUOUS_1_1 = {"type": "continuous", "min": -1.0, "max": 1.0}

FEATURES = [
    {
        "name": "close",
        "source": {"PriceField": "close"},
        "normalizer": "None",
        "clip_range": None,
    },
]


def test_constructor_accepts_swap_wallet():
    d = worker.LiveDriver(_config(), ["BTCUSDT"])
    assert type(d).__name__ == "LiveDriver"


def test_constructor_rejects_spot():
    with pytest.raises(ValueError, match="永续"):
        worker.LiveDriver(_config(), ["BTCUSDT"], futures=False)


def test_constructor_rejects_empty_symbols():
    with pytest.raises(ValueError, match="至少需要一个交易品种"):
        worker.LiveDriver(_config(), [])


def test_constructor_rejects_cash_accounting():
    with pytest.raises(ValueError, match="钱包"):
        worker.LiveDriver(_config(), ["BTCUSDT"], accounting="cash")


def test_constructor_rejects_negative_cash():
    with pytest.raises(ValueError, match="initial_cash"):
        worker.LiveDriver(_config(), ["BTCUSDT"], initial_cash=-1.0)


def test_add_rl_strategy_registers():
    d = worker.LiveDriver(_config(), ["BTCUSDT"])
    d.add_rl_strategy(
        lambda obs: 0.5, "m1", "v1-rl", 1, FEATURES, CONTINUOUS_1_1, "BTC/USDT"
    )


def test_add_rl_strategy_rejects_non_callable():
    d = worker.LiveDriver(_config(), ["BTCUSDT"])
    with pytest.raises(TypeError, match="predict"):
        d.add_rl_strategy(0.5, "m1", "v1-rl", 1, FEATURES, CONTINUOUS_1_1, "BTC/USDT")


def test_start_without_host_fails_fast(tmp_path):
    d = worker.LiveDriver(_config(), ["BTCUSDT"])
    with pytest.raises((ValueError, RuntimeError), match="至少需要一个策略宿主"):
        d.start(str(tmp_path / "live.journal"))


def test_risk_config_accepts_known_keys():
    """B3：已知配置键 + 金额上限在 start 前装配成功（构造期不发网络请求）"""
    d = worker.LiveDriver(_config(), ["BTCUSDT"])
    d.set_risk_config(
        {
            "max_order_value": 10_000.0,
            "max_daily_loss": 500.0,
            "max_leverage": 3.0,
            "max_drawdown": 0.10,
            "circuit_breaker_cooldown_secs": 60,
        },
        max_position_value=8_000.0,
    )


def test_risk_config_rejects_unknown_key():
    d = worker.LiveDriver(_config(), ["BTCUSDT"])
    with pytest.raises(ValueError, match="未知风控配置键"):
        d.set_risk_config({"max_daily_loss_x": 100.0})


def test_risk_pnl_reset_noop_without_config():
    """未配置风控时 pnl 回灌 / 跨日清零是 no-op（opt-in 契约，start 前也可调）"""
    d = worker.LiveDriver(_config(), ["BTCUSDT"])
    d.update_risk_pnl(-100.0)
    d.reset_risk_daily()
