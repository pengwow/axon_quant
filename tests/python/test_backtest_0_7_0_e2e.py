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
