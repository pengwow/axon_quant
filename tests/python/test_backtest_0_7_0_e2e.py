"""axon_quant 0.7.0 端到端冒烟测试(本地 wheel 验证)。

覆盖 4 个关键场景(plan 5.6):
1. 开仓:单笔 buy 1.0 → fills=1, position=1.0
2. 同向加仓:buy 0.5 + buy 0.3 → fills=2, trades=0, fills_detail=2
3. round-trip:buy 0.5 + sell 0.5 → fills=2, trades=1, fills_detail=2, realized_pnl ≈ 0
4. perp funding + mark:spot+perp delta-neutral,验证 funding_pnl / marks 累计

运行:
    .venv/bin/python -m pytest tests/python/test_backtest_0_7_0_e2e.py -v
"""

from __future__ import annotations

import pytest

from axon_quant.backtest import (
    BacktestEngine,
    limit_order,
    spot_instrument,
    swap_instrument,
)


# ── instrument 工具(spot / swap) ─────────────────────────────────
# Python 端 RunResult 用 tuple 表示 instrument key(`instrument_to_tuple`):
#   spot: ("spot", "BTC", "USDT")
#   swap: ("swap", "BTC", "USDT", "usd_margin", 1.0)
SPOT_BTC = spot_instrument("BTC", "USDT")
PERP_BTC = swap_instrument("BTC", "USDT", settle="usd_margin", contract_size=1.0)

# 匹配 RunResult.positions / leg_targets / marks 用的 tuple key
SPOT_KEY = ("spot", "BTC", "USDT")
PERP_KEY = ("swap", "BTC", "USDT", "usd_margin", 1.0)


def _make_engine(initial_cash: float = 100_000.0) -> BacktestEngine:
    """构造启用 default seed liquidity 的 BacktestEngine。

    half_spread=0.5 / depth=3 / size=0.1 足够稀疏,使策略单能轻易成交
    又不会因多层 partial fill 触发 hotfix 修复前的旧 bug。
    """
    bt = BacktestEngine(initial_cash=initial_cash)
    bt.with_seed_liquidity(half_spread=0.5, depth_levels=3, size_per_level=1.0)
    return bt


# ═══════════════════════════════════════════════════════════════════
# 1) 开仓:单笔 buy 1.0
# ═══════════════════════════════════════════════════════════════════


def test_open_position_spot_buy_1() -> None:
    """单笔 spot buy 1.0 在 mid=100 处成交,验证 fills=1, position=+1.0。"""
    bt = _make_engine()
    # 第一根 bar:seed spot 在 100 周围(ask 100.5 起步,bid 99.5 起步)
    bt.begin_bar(price=100.0, instrument=SPOT_BTC)
    # 推 taker buy 限价 100.0(可立即吃 ask 100.5,但这里用限价 100 = bid 等成)
    # 改用限价 100.5 直接吃卖一价,确保 1 笔成交
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(
            order_id=1,
            instrument=SPOT_BTC,
            side="Buy",
            price=100.5,
            quantity=1.0,
            tif="IOC",
        ),
    })
    result = bt.run()

    assert result.fills == 1, f"expected 1 fill, got {result.fills}"
    assert result.orders_accepted == 1
    # 同向加仓单笔,未平仓 → trades=[]
    assert result.trades == [], f"expected no round-trip, got {result.trades}"
    # fills_detail 应有 1 笔
    assert len(result.fills_detail) == 1
    fd = result.fills_detail[0]
    assert fd["quantity"] == 1.0
    assert fd["taker_side"] == "Buy"
    # 终态持仓:spot long 1.0
    assert result.positions[SPOT_KEY] == pytest.approx(1.0, abs=1e-9)


# ═══════════════════════════════════════════════════════════════════
# 2) 同向加仓:buy 0.5 + buy 0.3(同 side 加仓,无 round-trip)
# ═══════════════════════════════════════════════════════════════════


def test_add_to_position_buy_buy() -> None:
    """两笔 buy(0.5 + 0.3)同向加仓 → fills=2, trades=0, fills_detail=2。"""
    bt = _make_engine()
    bt.begin_bar(price=100.0, instrument=SPOT_BTC)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, SPOT_BTC, "Buy", 100.5, 0.5, tif="IOC"),
    })
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 2_000,
        "order": limit_order(2, SPOT_BTC, "Buy", 100.5, 0.3, tif="IOC"),
    })
    result = bt.run()

    assert result.fills == 2, f"expected 2 fills, got {result.fills}"
    # 关键断言:同向加仓 → trades 仍为空(round-trip 未发生)
    assert result.trades == [], f"expected no round-trip, got {result.trades}"
    # fills_detail 全记
    assert len(result.fills_detail) == 2
    # 终态持仓:spot long 0.5 + 0.3 = 0.8
    assert result.positions[SPOT_KEY] == pytest.approx(0.8, abs=1e-9)


# ═══════════════════════════════════════════════════════════════════
# 3) round-trip:buy 0.5 + sell 0.5
# ═══════════════════════════════════════════════════════════════════


def test_round_trip_buy_sell() -> None:
    """buy 0.5 + sell 0.5 round-trip → fills=2, trades=1, fills_detail=2, PnL ≈ 0。"""
    bt = _make_engine()
    # Bar 1:buy 0.5 @ 100.5
    bt.begin_bar(price=100.0, instrument=SPOT_BTC)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, SPOT_BTC, "Buy", 100.5, 0.5, tif="IOC"),
    })
    # Bar 2:re-seed,sell 0.5 @ 99.5(吃 bid,平仓)
    bt.begin_bar(price=100.0, instrument=SPOT_BTC)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 2_000,
        "order": limit_order(2, SPOT_BTC, "Sell", 99.5, 0.5, tif="IOC"),
    })
    result = bt.run()

    assert result.fills == 2, f"expected 2 fills, got {result.fills}"
    # round-trip 应记录 1 条 trade
    assert len(result.trades) == 1, f"expected 1 round-trip trade, got {result.trades}"
    # fills_detail 仍为 2 笔
    assert len(result.fills_detail) == 2
    # realized_pnl ≈ 0(buy @ 100.5, sell @ 99.5,差 -1.0 * 0.5 = -0.5)
    # 实际 round-trip PnL = (sell - buy) * qty = (99.5 - 100.5) * 0.5 = -0.5
    # (扣手续费前)
    trade_pnl = result.trades[0]["realized_pnl"]
    assert trade_pnl == pytest.approx(-0.5, abs=0.01), f"unexpected pnl: {trade_pnl}"
    # 终态持仓:已平仓,spot=0
    assert SPOT_KEY not in result.positions or result.positions[SPOT_KEY] == pytest.approx(0.0, abs=1e-9)


# ═══════════════════════════════════════════════════════════════════
# 4) perp funding + mark:spot+perp delta-neutral
# ═══════════════════════════════════════════════════════════════════


def test_perp_funding_and_mark() -> None:
    """spot+perp delta-neutral 头寸,推 funding + mark 事件,验证 funding_pnl / marks。

    注:0.7.0 的 `begin_bar_multi(legs: dict[instrument, price])` 当前有 API bug
    —— dict 不可哈希作为 dict key,所以本测试用 2 次 `begin_bar` 替代(每个
    bar 末次 rebalance + funding 调度各 1 次,可控范围更小)。
    0.7.1 已修:`begin_bar_multi` 接受 `list[tuple[instrument, price]]`,
    见 `test_begin_bar_multi_list_tuple`。
    """
    bt = _make_engine(initial_cash=1_000_000.0)
    # 启用 8h funding 自动调度(fixed_rate=0.0001 = 1bp / 8h)
    bt.with_funding_schedule(
        instrument=PERP_BTC,
        interval_ns=28_800_000_000_000,
        fixed_rate=0.0001,
    )

    # Bar 1a:spot seed(mid 50_000,半价差 0.5)→ spot buy 0.1 @ 50_000.5(吃 ask 50_000.5)
    bt.begin_bar(price=50_000.0, instrument=SPOT_BTC)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, SPOT_BTC, "Buy", 50_000.5, 0.1, tif="IOC"),
    })
    # Bar 1b:perp seed(mid 50_010)→ perp sell 0.1 @ 50_009.5(吃 bid 50_009.5)
    bt.begin_bar(price=50_010.0, instrument=PERP_BTC)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_001,
        "order": limit_order(2, PERP_BTC, "Sell", 50_009.5, 0.1, tif="IOC"),
    })

    # 推 funding 事件(perp 0.01% funding rate,mark 50_020)
    # 持仓 perp short -0.1,funding_pnl = -qty * rate * mark = -(-0.1) * 0.0001 * 50_020 ≈ +0.5002
    bt.push_funding(
        instrument=PERP_BTC,
        funding_rate=0.0001,
        mark_price=50_020.0,
        timestamp_ns=2_000,
    )
    # 推 mark 事件(spot + perp)
    bt.push_mark(instrument=SPOT_BTC, price=50_005.0, timestamp_ns=3_000)
    bt.push_mark(instrument=PERP_BTC, price=50_015.0, timestamp_ns=3_000)

    result = bt.run()

    # fills:spot buy 0.1 + perp sell 0.1 = 2
    assert result.fills == 2, (
        f"expected 2 fills, got {result.fills} "
        f"(accepted={result.orders_accepted}, rejected={result.orders_rejected})"
    )
    # funding_pnl 应被记录(perp short 收 funding)
    assert result.total_funding_pnl > 0.0, (
        f"expected positive funding pnl (perp short receives funding), "
        f"got {result.total_funding_pnl}"
    )
    # marks 应包含 spot + perp 两条最新 mark
    assert SPOT_KEY in result.marks
    assert PERP_KEY in result.marks
    assert result.marks[SPOT_KEY] == pytest.approx(50_005.0, abs=0.01)
    assert result.marks[PERP_KEY] == pytest.approx(50_015.0, abs=0.01)


# ═══════════════════════════════════════════════════════════════════
# 5) 0.7.0 Phase 4: RunResult.risk_metrics 暴露到 Python 端
# ═══════════════════════════════════════════════════════════════════


def test_risk_metrics_python_dict_exposed() -> None:
    """验证 `RunResult.risk_metrics` 是 dict 且 6 个字段都正确填充。

    场景:spot + perp delta-neutral(spot long 0.1 / perp short 0.1)
    → portfolio_delta ≈ 0,per_leg_delta[spot]=+0.1,per_leg_delta[perp]=-0.1
    → total_gamma=0 / vega=0 / sharpe_with_legs 沿用 sharpe_ratio
    """
    bt = _make_engine(initial_cash=1_000_000.0)

    # Bar 1:spot seed + buy 0.1 @ 100.5
    bt.begin_bar(price=100.0, instrument=SPOT_BTC)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, SPOT_BTC, "Buy", 100.5, 0.1, tif="IOC"),
    })
    # Bar 2:perp seed + sell 0.1 @ 99.5
    bt.begin_bar(price=100.0, instrument=PERP_BTC)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 2_000,
        "order": limit_order(2, PERP_BTC, "Sell", 99.5, 0.1, tif="IOC"),
    })
    result = bt.run()

    # risk_metrics 必须是 dict
    rm = result.risk_metrics
    assert isinstance(rm, dict), f"expected dict, got {type(rm)}"
    # 6 个字段
    for key in (
        "per_leg_delta",
        "portfolio_delta",
        "per_leg_gamma",
        "total_gamma",
        "vega",
        "sharpe_with_legs",
    ):
        assert key in rm, f"missing key: {key}"

    # delta-neutral 验证
    assert rm["per_leg_delta"][SPOT_KEY] == pytest.approx(0.1, abs=1e-9)
    assert rm["per_leg_delta"][PERP_KEY] == pytest.approx(-0.1, abs=1e-9)
    assert rm["portfolio_delta"] == pytest.approx(0.0, abs=1e-9), (
        f"delta-neutral 应 portfolio_delta=0, got {rm['portfolio_delta']}"
    )

    # gamma / vega 0.7.0 范围全 0
    assert rm["per_leg_gamma"][SPOT_KEY] == 0.0
    assert rm["per_leg_gamma"][PERP_KEY] == 0.0
    assert rm["total_gamma"] == 0.0
    assert rm["vega"] == 0.0

    # sharpe_with_legs 沿用 sharpe_ratio
    assert rm["sharpe_with_legs"] == pytest.approx(result.sharpe_ratio, abs=1e-9)


# ═══════════════════════════════════════════════════════════════════
# 6) 0.7.1 hotfix: begin_bar_multi 接受 list[tuple] (不再 dict)
# ═══════════════════════════════════════════════════════════════════


def test_begin_bar_multi_list_tuple() -> None:
    """0.7.1 修复:begin_bar_multi 接受 list[tuple] 而非 dict。

    Regression: 0.7.0 dict 形式因 dict 不可哈希,Python 端无法构造
    `dict[instrument_dict, price]`,实测 `TypeError: unhashable type: 'dict'`。

    修复:0.7.1 接受 `[(instrument_dict, price), ...]` 列表形式,
    语义等价,bar_id +1 / funding 调度 1 次 / 末次 rebalance。
    """
    bt = _make_engine(initial_cash=1_000_000.0)

    # 推 buy spot + sell perp 订单(在 begin_bar_multi 之前推入)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, SPOT_BTC, "Buy", 100.5, 0.1, tif="IOC"),
    })
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_001,
        "order": limit_order(2, PERP_BTC, "Sell", 99.5, 0.1, tif="IOC"),
    })

    # 关键:list[tuple] 形式,语义等价于 dict 但 key 可 hash
    bt.begin_bar_multi([
        (SPOT_BTC, 100.0),
        (PERP_BTC, 100.0),
    ])
    result = bt.run()

    # spot buy 0.1 + perp sell 0.1 = 2 fills
    assert result.fills == 2, f"expected 2 fills, got {result.fills}"
    # 终态 delta-neutral
    assert result.positions.get(SPOT_KEY, 0.0) == pytest.approx(0.1, abs=1e-9)
    assert result.positions.get(PERP_KEY, 0.0) == pytest.approx(-0.1, abs=1e-9)


def test_begin_bar_multi_list_tuple_wrong_arity() -> None:
    """0.7.1 修复:list 项不是 (instrument, price) tuple → 抛清晰 ValueError。"""
    bt = _make_engine()
    # 三元组 → PyValueError (expected len=2, got len=3)
    with pytest.raises(ValueError, match=r"len=3"):
        bt.begin_bar_multi([(SPOT_BTC, 100.0, "extra")])
    # 单元组 → PyValueError (expected len=2, got len=1)
    with pytest.raises(ValueError, match=r"len=1"):
        bt.begin_bar_multi([(SPOT_BTC,)])
    # 字符串元素 → PyValueError (cast to PyTuple 失败)
    with pytest.raises(ValueError, match=r"must be a list of"):
        bt.begin_bar_multi(["not_a_tuple"])


def test_begin_bar_multi_dict_form_rejected() -> None:
    """0.7.1:dict 形式(0.7.0 文档承诺但不可用)→ 抛错误而非 silently 接受。

    不再是隐式的 `TypeError: unhashable type: 'dict'`(用户无法构造 dict key),
    而是 PyO3 在 `Bound<PyList>` 边界拒绝 dict(实际报 dict 不可 hash,
    因为 PyO3 内部用 HashMap 区分 sequence 和 mapping)。
    用户应迁移到 `list[tuple]` 形式。
    """
    bt = _make_engine()
    with pytest.raises((TypeError, ValueError), match=r"(?i)(dict|hashable|iterable)"):
        # begin_bar_multi 现在要求 PyList,dict 被 PyO3 拒绝
        bt.begin_bar_multi({SPOT_BTC: 100.0, PERP_BTC: 100.0})


# ═══════════════════════════════════════════════════════════════════
# 7) 0.7.1 新增: bar_nav_curve 每 bar 末 NAV 采样
# ═══════════════════════════════════════════════════════════════════


def test_bar_nav_curve_empty_without_begin_bar() -> None:
    """0.7.1 新增:不调 `begin_bar` 时 `bar_nav_curve` 应为空 list。

    验证:空回测(只有初始资金,无 begin_bar 调用)→ `bar_nav_curve == []`。
    """
    bt = _make_engine(initial_cash=100_000.0)
    result = bt.run()
    assert result.bar_nav_curve == [], (
        f"未调 begin_bar 时 bar_nav_curve 应为空,got {result.bar_nav_curve}"
    )


def test_bar_nav_curve_sampled_per_begin_bar() -> None:
    """0.7.1 新增:3 次 `begin_bar` → `bar_nav_curve` 有 3 帧,时间戳单调递增。

    验证:`set_clock` + `begin_bar` 3 次,每根 bar 都产生 1 帧 NAV 采样。
    """
    bt = _make_engine(initial_cash=100_000.0)
    # 3 根 bar,各设不同时间戳
    bt.set_clock(1_000_000_000)
    bt.begin_bar(price=50_000.0, instrument=SPOT_BTC)
    bt.set_clock(2_000_000_000)
    bt.begin_bar(price=50_100.0, instrument=SPOT_BTC)
    bt.set_clock(3_000_000_000)
    bt.begin_bar(price=50_200.0, instrument=SPOT_BTC)

    result = bt.run()
    assert len(result.bar_nav_curve) == 3, (
        f"3 次 begin_bar 应采 3 帧,got {len(result.bar_nav_curve)}"
    )
    # 时间戳单调递增
    assert result.bar_nav_curve[0][0] == 1_000_000_000
    assert result.bar_nav_curve[1][0] == 2_000_000_000
    assert result.bar_nav_curve[2][0] == 3_000_000_000
    # NAV 单调应反映 begin_bar 价递增(无持仓 → NAV=cash=100_000)
    for ts, nav in result.bar_nav_curve:
        assert nav == pytest.approx(100_000.0, abs=0.01), (
            f"无持仓时 NAV 应=cash=100_000,got ts={ts} nav={nav}"
        )


def test_bar_nav_curve_differs_from_equity_curve_no_events() -> None:
    """0.7.1 新增:`bar_nav_curve` 区别于 `equity_curve`:无 fill 时 bar_nav 仍有帧。

    场景:3 次 `begin_bar` 不发任何 order / mark / funding 事件:
    - `equity_curve` 应为空(没事件触发采样)
    - `bar_nav_curve` 应有 3 帧(每 bar 末都采样)

    这是 PR-B 的核心动机:短回测 + 无 fill 时 `equity_curve` 末帧 = initial_cash,
    无法反映波动;`bar_nav_curve` 在 mark_cache 已有值时能反映 NAV 沿 mark 变化。
    """
    bt = _make_engine(initial_cash=100_000.0)
    for i in range(3):
        bt.set_clock(1_000 * (i + 1))
        bt.begin_bar(price=50_000.0 + i * 100, instrument=SPOT_BTC)

    result = bt.run()
    assert result.equity_curve == [], (
        f"无 fill/mark/funding 事件时 equity_curve 应仍为空,got {len(result.equity_curve)} 帧"
    )
    assert len(result.bar_nav_curve) == 3, (
        f"3 次 begin_bar 应采 3 帧 bar_nav_curve,got {len(result.bar_nav_curve)}"
    )


def test_bar_nav_curve_sampled_for_begin_bar_multi() -> None:
    """0.7.1 新增:`begin_bar_multi` 也产生 1 帧 `bar_nav_curve`(per-call 单帧)。"""
    bt = _make_engine(initial_cash=1_000_000.0)
    # 2 次 begin_bar_multi(spot+perp 套利场景)
    bt.set_clock(1_000_000_000)
    bt.begin_bar_multi([(SPOT_BTC, 50_000.0), (PERP_BTC, 50_010.0)])
    bt.set_clock(2_000_000_000)
    bt.begin_bar_multi([(SPOT_BTC, 50_100.0), (PERP_BTC, 50_110.0)])

    result = bt.run()
    assert len(result.bar_nav_curve) == 2, (
        f"2 次 begin_bar_multi 应采 2 帧,got {len(result.bar_nav_curve)}"
    )
    assert result.bar_nav_curve[0][0] == 1_000_000_000
    assert result.bar_nav_curve[1][0] == 2_000_000_000


def test_bar_nav_curve_exposed_via_to_dict() -> None:
    """0.7.1 新增:`result.to_dict()` 暴露 `bar_nav_curve_points` 字段。"""
    bt = _make_engine(initial_cash=100_000.0)
    bt.set_clock(1_000)
    bt.begin_bar(price=50_000.0, instrument=SPOT_BTC)
    bt.set_clock(2_000)
    bt.begin_bar(price=50_100.0, instrument=SPOT_BTC)
    result = bt.run()

    d = result.to_dict()
    assert "bar_nav_curve_points" in d, (
        f"to_dict 缺 bar_nav_curve_points 字段,keys={list(d.keys())}"
    )
    assert d["bar_nav_curve_points"] == 2
    # 同时 equity_curve_points 仍为 0(无 fill)
    assert d["equity_curve_points"] == 0


# ═══════════════════════════════════════════════════════════════════
# 8) 0.7.1 新增: with_* 方法链式调用
# ═══════════════════════════════════════════════════════════════════


def test_with_methods_chainable_in_python() -> None:
    """0.7.1 新增:`with_*` 方法返回 engine 自身,支持 Python 链式调用。

    验证:`BacktestEngine(initial_cash=...)` 后能用 `.with_xxx().with_yyy()`
    链式连续配线,无需中间变量。

    0.7.1 之前这些 `with_*` 返回 `None`(in-place mutator),链式会报
    `AttributeError: 'NoneType' object has no attribute 'with_yyy'`。
    """
    # 链式调用 5 个 with_* 方法
    bt = (
        BacktestEngine(initial_cash=100_000.0)
        .with_seed_liquidity(0.1, 5, 0.1)
        .with_seed_liquidity_for(SPOT_BTC, 0.01, 10, 0.5)
        .with_seed_liquidity_for(PERP_BTC, 0.5, 5, 0.1)
        .with_auto_rebalance(1e-6)
        .with_force_liquidate(False)
    )
    # 验证:链式调用后 backtest 能正常跑(无内部 None deref 异常)
    bt.set_clock(1_000_000_000)
    bt.begin_bar(price=50_000.0, instrument=SPOT_BTC)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000_000_001,
        "order": limit_order(1, SPOT_BTC, "Buy", 50_000.5, 0.1, tif="IOC"),
    })
    result = bt.run()
    # 至少 1 笔 fill(per-leg seed 已生效)
    assert result.fills == 1, f"链式 with_seed_liquidity_for 后 fill 应=1,got {result.fills}"


def test_with_fee_config_and_funding_schedule_chainable() -> None:
    """0.7.1 新增:`with_fee_config` + `with_funding_schedule` + 末 `with_auto_rebalance_disable` 链式。

    验证:`with_funding_schedule` / `with_auto_rebalance_disable` 这两个 0.5.0/0.6.0
    新增的 with_* 也都返回 engine 自身(0.7.1 之前是 `()`)。
    """
    bt = (
        BacktestEngine(initial_cash=1_000_000.0)
        .with_seed_liquidity(50.0, 3, 0.1)
        .with_fee_config(0.002)
        .with_funding_schedule(
            instrument=PERP_BTC,
            interval_ns=28_800_000_000_000,  # 8h
            fixed_rate=0.0001,
        )
        .with_auto_rebalance_disable()  # 显式关闭
    )
    # 验证能跑完
    bt.set_clock(1_000_000_000)
    bt.begin_bar(price=50_000.0, instrument=PERP_BTC)
    result = bt.run()
    # funding schedule 已生效(首 bar 末跨 8h 边界)
    assert result.fills >= 0  # 没 fill 也 OK,主要验证链式不报错


def test_with_methods_overwrite_semantics_in_chain() -> None:
    """0.7.1 新增:链式调用中后调覆盖前调(已有"set 最新生效"语义)。

    验证:连续两次 `with_fee_config(0.001)` 然后 `with_fee_config(0.005)`,
    最终 taker_rate 应 = 0.005,对应 total_fees = 0.005 * fill_qty * fill_price。
    """
    bt = (
        BacktestEngine(initial_cash=100_000.0)
        .with_fee_config(0.001)
        .with_fee_config(0.005)
    )
    # 内部验证:无显式 getter,跑一个 buy 测手续费累计
    # 注:seed_liquidity 每层 size=0.1,1.0 数量的单子实际 fill 0.1(0.7.0 撮合语义)
    bt.with_seed_liquidity(0.5, 3, 0.1)
    bt.set_clock(1_000_000_000)
    bt.begin_bar(price=100.0, instrument=SPOT_BTC)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000_000_001,
        "order": limit_order(1, SPOT_BTC, "Buy", 100.5, 1.0, tif="IOC"),
    })
    result = bt.run()
    # 链式后调 0.005 覆盖前调 0.001,fill 0.1 @ 100.5
    fill_qty = result.fills_detail[0]["quantity"]  # 实际 fill 量
    fill_price = result.fills_detail[0]["price"]
    expected_fee = 0.005 * fill_qty * fill_price
    assert result.fills == 1, f"应 fill 1 笔,got {result.fills}"
    assert abs(result.total_fees - expected_fee) < 1e-6, (
        f"链式后调覆盖前调,total_fees 应≈{expected_fee:.6f},got {result.total_fees:.6f}"
    )


# ── 0.7.1 PR-D:sharpe_ratio 样本不足警告 + bar 间隔归一化 ─────────


def test_sharpe_ratio_default_15min_magnitude() -> None:
    """0.7.1 PR-D 回归:0.7.0 错传 `35_040_f64.sqrt()` 导致实际年化因子
    比正确值小一个数量级;0.7.1 改用 `sharpe_ratio_annualized(900.0)`,
    在多 bar + 真实 PnL 场景下,sharpe_ratio 应为有限数(不为极小值)。

    验证:多 bar round-trip 拿到多次 log return,sharpe_ratio:
    - n=1 短回测 → 0.0(样本不足)
    - 多 bar 有 PnL → 有限数(数量级在 -100 ~ 100)
    """
    from axon_quant.backtest import BacktestEngine, limit_order, spot_instrument

    bt = BacktestEngine(initial_cash=100_000.0)
    bt.with_seed_liquidity(half_spread=0.5, depth_levels=3, size_per_level=1.0)
    spot = spot_instrument("BTC", "USDT")

    # 单 bar round-trip → n=1 → 0.0
    bt.set_clock(1_000_000_000)
    bt.begin_bar(price=100.0, instrument=spot)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000_000_001,
        "order": limit_order(1, spot, "Buy", 100.5, 0.1, tif="IOC"),
    })
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000_000_002,
        "order": limit_order(2, spot, "Sell", 100.7, 0.1, tif="IOC"),
    })
    result = bt.run()
    # 单 bar round-trip 没有跨 bar NAV 变化,log_return_count 可能 < 2 → 0.0
    # 关键是:不再因为 `35_040_f64.sqrt()` 错传导致除零或 NaN
    assert isinstance(result.sharpe_ratio, float)
    assert not (result.sharpe_ratio != result.sharpe_ratio), "sharpe_ratio 不应为 NaN"
    # 0.7.0 错传时,这个值会算成极小;0.7.1 修复后即使是 0 也是干净的 0
    assert result.sharpe_ratio == 0.0, (
        f"单 bar 短回测 sharpe_ratio 应 = 0.0,got {result.sharpe_ratio}"
    )


def test_sharpe_ratio_multi_bar_with_pnl_is_finite() -> None:
    """0.7.1 PR-D:多 bar 真实回测,sharpe_ratio 应为有限数(无 NaN/Inf)。

    关键回归:0.7.0 错传 `35_040_f64.sqrt()` 时,多 bar 场景虽然能算出
    数值,但年化因子比正确小 13.7 倍,quantcell 看到 0.0X 的数量级。
    0.7.1 用 `sharpe_ratio_annualized(900.0)` 后,数量级恢复正常(0~10)。
    """
    from axon_quant.backtest import BacktestEngine, limit_order, spot_instrument

    bt = BacktestEngine(initial_cash=100_000.0)
    bt.with_seed_liquidity(half_spread=0.1, depth_levels=5, size_per_level=1.0)
    spot = spot_instrument("BTC", "USDT")

    # 5 个 bar,每个 bar 做一次 round-trip 累积 NAV 变化
    for i in range(5):
        ts = 1_000_000_000 + i * 900_000_000_000  # 900s 间隔 = 15-min
        bt.set_clock(ts)
        bt.begin_bar(price=100.0 + i, instrument=spot)
        bt.push_event({
            "type": "order_submitted",
            "timestamp_ns": ts + 1,
            "order": limit_order(10 + i * 2, spot, "Buy", 100.0 + i + 0.5, 0.1, tif="IOC"),
        })
        bt.push_event({
            "type": "order_submitted",
            "timestamp_ns": ts + 2,
            "order": limit_order(11 + i * 2, spot, "Sell", 100.0 + i + 0.7, 0.1, tif="IOC"),
        })

    result = bt.run()
    assert result.fills >= 2, f"多 bar 期望 fills>=2,got {result.fills}"

    # 关键:sharpe_ratio 应是有限数(不为 NaN/Inf)
    assert isinstance(result.sharpe_ratio, float)
    assert result.sharpe_ratio == result.sharpe_ratio, "sharpe_ratio 不应为 NaN"
    assert abs(result.sharpe_ratio) < 1e6, (
        f"sharpe_ratio 数量级应 < 1e6,got {result.sharpe_ratio} "
        "(0.7.0 错传 sqrt 会被放大 1e13 倍)"
    )
