"""PaperDriver 绑定层冒烟验证（Task 11.4：RL paper 循环）。

覆盖不联网即可验证的完整边界：构造校验（空品种 / 非法 initial_cash /
fee_rate）、RL 策略注册、无宿主 / 未 start 的 fail-fast，以及 feed_bar
全链路的闭式账本断言（建仓 → 盯市 → 清仓）。paper 账纯内存、无外部
连接，测试直接灌收市 bar 驱动。

成交价口径：PyPaperDriver 未暴露 seed 参数，PaperSink 用 SeedLiquidity
默认 half_spread=0.0005（绝对价差）种盘口——买单吃 ask=mid+0.0005、
卖单吃 bid=mid−0.0005；SetTarget 换算 qty = ratio × equity / close。
"""

import time

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


def test_rule_strategy_action_object_produces_fill():
    """规则策略返回 Action 对象（官方契约）→ 首根 bar 建仓必须成交。

    回归（两个缺陷都表现为零成交、无报错）：
    1. PyAction::new 的 action_type 只收 str，官方示例传 ActionType
       枚举直接 TypeError（策略构造即崩）；
    2. PythonStrategyImpl 解析 action_type 时 extract::<String> 对
       PyActionType pyclass 枚举实例失败，曾静默降级 Hold。

    内核契约：add_strategy 收到的 target_position 是**绝对数量**
    （ratio→数量的换算在 QuantCell 侧 RustStrategyAdapter，内核不做）；
    策略对象需带 `symbol` 属性（wrap_strategy 以此确定品种路由）。
    bar0 close=100，目标 10 BTC：买单价 = 100 + 0.0005（绝对价差），
    equity = (10000 − 10×100.0005) + 10×100 = 9999.995。
    """
    from axon_quant import Action, ActionType

    d = worker.PaperDriver(["BTCUSDT"], initial_cash=10_000.0, fee_rate=0.0)

    class _BuyOnce:
        symbol = "BTC/USDT"

        def on_bar(self, bar):
            if bar["close"] <= 100.0:
                # target_position = 10（绝对数量 BTC），非权益占比
                return Action(ActionType.Buy, 0.8, 10.0, "rule_smoke", 0)
            return Action(ActionType.Hold, 0.0, 0.0, "rule_smoke", 0)

    d.add_strategy(_BuyOnce())
    d.start()

    d.feed_bar("BTCUSDT", START, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    d.tick()
    snap = d.snapshot()
    assert snap["positions"]["BTC/USDT"] == pytest.approx(10.0, abs=1e-4)
    assert snap["equity"] == pytest.approx(9_999.995, abs=1e-4)


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


def test_risk_pnl_reset_noop_without_config():
    """未配置风控时 pnl 回灌 / 跨日清零是 no-op（opt-in 契约）"""
    d = worker.PaperDriver(["BTCUSDT"])
    d.update_risk_pnl(-100.0)
    d.reset_risk_daily()


def test_risk_config_rejects_unknown_key():
    """未知配置键必须报错，防拼错键名静默走默认值"""
    d = worker.PaperDriver(["BTCUSDT"])
    with pytest.raises(ValueError, match="未知风控配置键"):
        d.set_risk_config({"max_daily_loss_x": 100.0})


def test_risk_pnl_backfill_blocks_expansion_allows_exit():
    """B3 端到端：pnl 回灌联动熔断 → 扩仓 tick 拒（risk rejected）→ 平仓放行。

    max_daily_loss=5：bar0 ratio=0.1 建仓 10 股（名义 1000 过检），回灌
    −6 触发熔断；bar1 ratio=0.5 扩仓（target 50 股 > 10 → 走引擎链）被
    熔断拒单，tick 上抛 RuntimeError；bar2 ratio=0 平仓在引擎链前放行，
    账本回到空仓（fee=0：卖单价 99.9995，equity = 9999.99）。
    """
    d = worker.PaperDriver(["BTCUSDT"], initial_cash=10_000.0, fee_rate=0.0)
    ratios = iter([0.1, 0.5, 0.0])
    d.add_rl_strategy(
        lambda obs: next(ratios), "m1", "v1-rl", 1, FEATURES, CONTINUOUS_1_1, "BTC/USDT"
    )
    d.set_risk_config({"max_daily_loss": 5.0}, max_position_value=50_000.0)
    d.start()

    # bar0 建仓 0.1
    d.feed_bar("BTCUSDT", START, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    d.tick()
    assert d.snapshot()["positions"]["BTC/USDT"] == pytest.approx(10.0, abs=1e-4)

    # 回灌当日亏损 → 熔断
    d.update_risk_pnl(-6.0)

    # bar1 扩仓被拒
    d.feed_bar("BTCUSDT", START + ONE_MIN, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    with pytest.raises(RuntimeError, match="risk rejected"):
        d.tick()

    # bar2 平仓放行（熔断下唯一出场通道）
    d.feed_bar("BTCUSDT", START + 2 * ONE_MIN, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    d.tick()
    snap = d.snapshot()
    assert snap["positions"] == {}
    assert snap["equity"] == pytest.approx(9_999.99, abs=1e-4)

    # 跨日清零恢复后调用不抛错（无风控句柄残留锁死的兜底口径）
    d.reset_risk_daily()


def test_risk_assembled_lifecycle_and_runtime_toggle():
    """热更三方法端到端：assembled 探测 / 停用 bypass / 重启用复挡 / 热更阈值。

    对照 test_risk_pnl_backfill_blocks_expansion_allows_exit（同熔断档下扩仓
    必拒），本测试补充运行时开关三语义：
    1. 停用 bypass 全放行——熔断激活态下扩仓同笔放行（bar2）；
    2. 重启用清 bypass——熔断激活态回到挡单（bar3），证明开关实时生效；
    3. 热更 max_daily_loss 放宽不追溯已激活熔断（bar4 仍拒）；cooldown
       收 0 后激活态立即失效、新阈值实时生效（bar5 扩仓放行）。
    set_risk_enabled 未装配时 no-op（构造后先调一次，opt-in 契约同
    test_risk_pnl_reset_noop_without_config）。
    """
    d = worker.PaperDriver(["BTCUSDT"], initial_cash=10_000.0, fee_rate=0.0)
    assert d.risk_assembled() is False

    ratios = iter([0.1, 0.5, 0.5, 0.9, 0.9, 0.9])
    d.add_rl_strategy(
        lambda obs: next(ratios), "m1", "v1-rl", 1, FEATURES, CONTINUOUS_1_1, "BTC/USDT"
    )
    d.set_risk_enabled(False)  # 未装配 no-op：不抛错即契约
    d.set_risk_config({"max_daily_loss": 5.0}, max_position_value=50_000.0)
    # start 前仅存 pending 配置（set_risk 不建句柄），风险句柄 start 时装配
    assert d.risk_assembled() is False
    d.start()
    assert d.risk_assembled() is True

    # bar0 建仓 10 股，回灌 −6 触发熔断（与参照测试同档）
    d.feed_bar("BTCUSDT", START, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    d.tick()
    assert d.snapshot()["positions"]["BTC/USDT"] == pytest.approx(10.0, abs=1e-4)
    d.update_risk_pnl(-6.0)

    # bar1 扩仓被熔断拒
    d.feed_bar("BTCUSDT", START + ONE_MIN, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    with pytest.raises(RuntimeError, match="risk rejected"):
        d.tick()

    # 停用：熔断态扩仓放行；停用期间 pnl 回灌照常累计（daily_pnl = −7）
    d.set_risk_enabled(False)
    d.update_risk_pnl(-1.0)
    d.feed_bar("BTCUSDT", START + 2 * ONE_MIN, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    d.tick()
    assert d.snapshot()["positions"]["BTC/USDT"] == pytest.approx(50.0, abs=1e-2)

    # 重启用：bypass 清除，熔断激活态恢复挡单
    d.set_risk_enabled(True)
    d.feed_bar("BTCUSDT", START + 3 * ONE_MIN, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    with pytest.raises(RuntimeError, match="risk rejected"):
        d.tick()

    # 热更放宽 max_daily_loss：阈值实时到位但不追溯已激活熔断
    d.update_risk_config({"max_daily_loss": 100.0}, max_position_value=None)
    d.feed_bar("BTCUSDT", START + 4 * ONE_MIN, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    with pytest.raises(RuntimeError, match="risk rejected"):
        d.tick()

    # 热更 cooldown=0：激活态立即失效 + 新 max_daily_loss 生效 → 扩仓放行。
    # breaker 自恢复是秒级粒度（now − activated > cooldown 需跨秒），
    # 与内核单测 update_cooldown_shortens_recovery 的 sleep 1.1s 同口径
    d.update_risk_config(
        {"max_daily_loss": 100.0, "circuit_breaker_cooldown_secs": 0},
        max_position_value=None,
    )
    time.sleep(1.1)
    d.feed_bar("BTCUSDT", START + 5 * ONE_MIN, 100.0, 101.0, 99.0, 100.0, 10_000.0)
    d.tick()
    snap = d.snapshot()
    assert snap["positions"]["BTC/USDT"] == pytest.approx(90.0, abs=0.5)
    assert snap["equity"] == pytest.approx(9_999.96, abs=0.02)
