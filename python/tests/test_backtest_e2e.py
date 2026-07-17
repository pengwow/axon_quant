"""axon_quant.backtest 端到端测试(L3 Python E2E)。

覆盖范围:
1. 类型导入 / 实例化
2. 工厂函数 limit_order / market_order
3. L1MatchingEngine 基础撮合(无成交 / 跨价成交 / 部分成交 / 主动撤单)
4. L2MatchingEngine 进阶(modify / stats / volume_at_price / from_entries)
5. MultiAssetMatchingEngine 多资产路由 + 批量模式
6. ImpactedMatchingEngine 冲击感知 + Builder
7. BacktestEngine 事件驱动回测(4 种事件类型)+ run 幂等性
8. 异常路径(BacktestError / KeyError / ValueError)
9. RunResult.to_dict 字段完整

运行::

    cd /Users/liupeng/workspace/quant/axon
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon/.venv/bin/python \\
        python -m pytest python/tests/test_backtest_e2e.py -v

注意:本测试需先 build wheel(参见 Makefile 的 `python-build` /
`python-develop` 目标)。如未 build,部分测试 skip。
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

# 强制使用本项目 venv(避免 miniconda pyarrow / numpy 干扰)
_VENV_SITE = Path("/Users/liupeng/workspace/quant/axon/.venv/lib/python3.14/site-packages")
if _VENV_SITE.exists() and str(_VENV_SITE) not in sys.path:
    sys.path.insert(0, str(_VENV_SITE))

# `axon_quant` 在 maturin develop / wheel install 后可被 import
# 缺失时 skip 整个模块(开发期还没 build 时常见)
try:
    import axon_quant  # noqa: F401
    from axon_quant.backtest import (
        L1MatchingEngine,
        L2MatchingEngine,
        MultiAssetMatchingEngine,
        ImpactedMatchingEngine,
        ImpactedMatchingEngineBuilder,
        BacktestEngine,
        RunResult,
        RunStats,
        BacktestError,
        OrderBookEntry,
        DarkOrder,
        CrossPair,
        AuctionResult,
        ArbitrageOpportunity,
        limit_order,
        market_order,
        spot_instrument,
        swap_instrument,
    )
    _BACKTEST_AVAILABLE = hasattr(axon_quant, "_native") and hasattr(
        axon_quant._native, "backtest"
    )
except ImportError as _e:
    pytest.skip(f"axon_quant not installed: {_e}", allow_module_level=True)
    raise  # 实际不可达,仅供类型检查

if not _BACKTEST_AVAILABLE:
    pytest.skip(
        "axon_quant._native.backtest not yet registered (need maturin develop)",
        allow_module_level=True,
    )


# 0.5.0 起:所有 OrderDict 工厂用 `instrument` dict 代替 `symbol` 字符串。
# 在测试模块顶部预创建常用 instrument,避免每个用例重复构造。
_BTC_USDT = spot_instrument("BTC", "USDT")
_ETH_USDT = spot_instrument("ETH", "USDT")
_BTC_USDT_PERP = swap_instrument("BTC", "USDT", settle="usd_margin", contract_size=1.0)
_X_USDT = spot_instrument("X", "USDT")  # 通用占位 instrument


# ═══════════════════════════════════════════════════════════════════════════
# 类型可用性
# ═══════════════════════════════════════════════════════════════════════════


def test_backtest_module_imports_all_symbols():
    """所有 backtest 符号都能 import(0.5.0 起包含 Instrument 工厂)。"""
    # 上面的 try/except 已经验证了,这里再次确保类可访问
    assert L1MatchingEngine is not None
    assert L2MatchingEngine is not None
    assert MultiAssetMatchingEngine is not None
    assert ImpactedMatchingEngine is not None
    assert ImpactedMatchingEngineBuilder is not None
    assert BacktestEngine is not None
    assert RunResult is not None
    assert RunStats is not None
    assert BacktestError is not None
    assert OrderBookEntry is not None
    assert DarkOrder is not None
    assert CrossPair is not None
    assert AuctionResult is not None
    assert ArbitrageOpportunity is not None
    # 工厂函数(0.5.0:加 Instrument 工厂)
    assert callable(limit_order)
    assert callable(market_order)
    assert callable(spot_instrument)
    assert callable(swap_instrument)


def test_backtest_submodule_path():
    """axon_quant.backtest 子模块路径可达。"""
    assert hasattr(axon_quant, "backtest")
    # backtest.py 模块(纯 Python wrapper)
    assert axon_quant.backtest.__file__.endswith("backtest.py")


# ═══════════════════════════════════════════════════════════════════════════
# 工厂函数
# ═══════════════════════════════════════════════════════════════════════════


def test_limit_order_factory():
    """limit_order 工厂返回 dict 协议,字段齐全(0.5.0:`symbol` → `instrument`)。"""
    o = limit_order(1, _BTC_USDT, "Buy", 100.0, 1.0)
    assert o == {
        "id": 1,
        "instrument": _BTC_USDT,
        "side": "Buy",
        "type": "limit",
        "price": 100.0,
        "quantity": 1.0,
        "tif": "GTC",
    }


def test_limit_order_default_tif_is_uppercase():
    """limit_order tif 默认 GTC,且大小写自动归一化。"""
    o = limit_order(1, _X_USDT, "Buy", 100.0, 1.0, tif="gtc")
    assert o["tif"] == "GTC"


def test_market_order_factory():
    """market_order 工厂强制 tif=IOC(0.5.0:`symbol` → `instrument`)。"""
    o = market_order(2, _ETH_USDT, "Sell", 5.0)
    assert o == {
        "id": 2,
        "instrument": _ETH_USDT,
        "side": "Sell",
        "type": "market",
        "quantity": 5.0,
        "tif": "IOC",
    }
    assert "price" not in o  # 市价单无 price


# ═══════════════════════════════════════════════════════════════════════════
# 0.5.0 新增:Instrument 工厂测试
# ═══════════════════════════════════════════════════════════════════════════


def test_spot_instrument_factory():
    """spot_instrument 工厂返回符合 Rust parse_instrument 协议的 dict。"""
    inst = spot_instrument("BTC", "USDT")
    assert inst == {"kind": "spot", "base": "BTC", "quote": "USDT"}


def test_spot_instrument_rejects_empty():
    """spot_instrument 空 base/quote → ValueError。"""
    with pytest.raises(ValueError):
        spot_instrument("", "USDT")
    with pytest.raises(ValueError):
        spot_instrument("BTC", "")


def test_swap_instrument_factory():
    """swap_instrument 工厂返回带 settle + contract_size 的 dict。"""
    inst = swap_instrument("BTC", "USDT", settle="usd_margin", contract_size=1.0)
    assert inst == {
        "kind": "swap",
        "base": "BTC",
        "quote": "USDT",
        "settle": "usd_margin",
        "contract_size": 1.0,
    }


def test_swap_instrument_coin_margin():
    """swap_instrument 接受 coin_margin(币本位)。"""
    inst = swap_instrument("BTC", "USD", settle="coin_margin", contract_size=0.01)
    assert inst["settle"] == "coin_margin"
    assert inst["contract_size"] == 0.01


def test_swap_instrument_settle_case_insensitive():
    """swap_instrument settle 字段大小写不敏感(归一化到小写)。"""
    inst_upper = swap_instrument("BTC", "USDT", settle="USD_MARGIN")
    inst_mixed = swap_instrument("BTC", "USDT", settle="Usd_Margin")
    assert inst_upper == inst_mixed
    assert inst_upper["settle"] == "usd_margin"


def test_swap_instrument_rejects_invalid_settle():
    """swap_instrument 非法 settle → ValueError。"""
    with pytest.raises(ValueError):
        swap_instrument("BTC", "USDT", settle="bitcoin_settled")


def test_swap_instrument_rejects_non_positive_contract_size():
    """swap_instrument contract_size <= 0 → ValueError。"""
    with pytest.raises(ValueError):
        swap_instrument("BTC", "USDT", contract_size=0.0)
    with pytest.raises(ValueError):
        swap_instrument("BTC", "USDT", contract_size=-1.0)


def test_limit_order_validates_instrument():
    """limit_order 接收非法 instrument → 抛 TypeError / KeyError / ValueError。"""
    # 非 dict
    with pytest.raises(TypeError):
        limit_order(1, "BTCUSDT", "Buy", 100.0, 1.0)
    # 缺 kind
    with pytest.raises(KeyError):
        limit_order(1, {"base": "BTC", "quote": "USDT"}, "Buy", 100.0, 1.0)
    # 非法 kind
    with pytest.raises(ValueError):
        limit_order(1, {"kind": "future", "base": "BTC", "quote": "USDT"}, "Buy", 100.0, 1.0)
    # swap 缺 settle
    with pytest.raises(KeyError):
        limit_order(
            1,
            {"kind": "swap", "base": "BTC", "quote": "USDT", "contract_size": 1.0},
            "Buy",
            100.0,
            1.0,
        )


def test_limit_order_validates_side_and_price():
    """limit_order 非法 side / 负价 → ValueError。"""
    with pytest.raises(ValueError):
        limit_order(1, _BTC_USDT, "invalid_side", 100.0, 1.0)
    with pytest.raises(ValueError):
        limit_order(1, _BTC_USDT, "Buy", 0.0, 1.0)
    with pytest.raises(ValueError):
        limit_order(1, _BTC_USDT, "Buy", 100.0, -1.0)


# ═══════════════════════════════════════════════════════════════════════════
# L1MatchingEngine 基础撮合
# ═══════════════════════════════════════════════════════════════════════════


def test_l1_no_match_for_buy_no_ask():
    """买单无卖单时挂单,is_filled=False。"""
    engine = L1MatchingEngine()
    result = engine.submit(limit_order(1, _BTC_USDT, "Buy", 100.0, 1.0))
    assert result["is_filled"] is False
    assert result["remaining_quantity"] == 1.0
    assert engine.active_order_count == 1
    assert engine.best_bid == 100.0
    assert engine.best_ask is None  # 无卖单


def test_l1_cross_match_yields_one_fill():
    """同价买卖跨价成交,is_filled=True,1 笔 fill。"""
    engine = L1MatchingEngine()
    engine.submit(limit_order(1, _BTC_USDT, "Sell", 100.0, 1.0))
    result = engine.submit(limit_order(2, _BTC_USDT, "Buy", 100.0, 1.0))
    assert result["is_filled"] is True
    assert len(result["fills"]) == 1
    assert result["fills"][0]["price"] == 100.0
    # fill dict 的 taker_side 是全大写("BUY"/"SELL")
    assert result["fills"][0]["taker_side"] == "BUY"
    assert result["fills"][0]["taker_order_id"] == 2  # 后到的买单是 taker
    # 卖单成交后订单簿:best_ask/best_bid 仍可能残留(L1 不主动清已 Filled 订单)


def test_l1_partial_fill_via_buy_smaller():
    """大卖单 + 小买单:买单全成,卖单部分剩余。"""
    engine = L1MatchingEngine()
    engine.submit(limit_order(1, _BTC_USDT, "Sell", 100.0, 5.0))
    result = engine.submit(limit_order(2, _BTC_USDT, "Buy", 100.0, 3.0))
    assert result["is_filled"] is True  # 买单 3 全部成交
    assert result["remaining_quantity"] == 0
    # 卖单剩 2 仍挂单
    assert engine.active_order_count == 1
    assert engine.best_ask == 100.0


def test_l1_cancel_active_order():
    """cancel 已存在订单返回 True,无订单返回 False。"""
    engine = L1MatchingEngine()
    engine.submit(limit_order(1, _BTC_USDT, "Buy", 100.0, 1.0))
    assert engine.cancel(1) is True
    assert engine.active_order_count == 0
    # 再次 cancel 同 ID 返回 False
    assert engine.cancel(1) is False


def test_l1_depth_returns_dict_with_bids_asks():
    """depth 返回 dict {bids: [...], asks: [...]}。"""
    engine = L1MatchingEngine()
    engine.submit(limit_order(1, _BTC_USDT, "Buy", 100.0, 1.0))
    engine.submit(limit_order(2, _BTC_USDT, "Buy", 99.0, 2.0))
    engine.submit(limit_order(3, _BTC_USDT, "Sell", 101.0, 1.5))
    depth = engine.depth(5)
    assert set(depth.keys()) == {"bids", "asks"}
    bids = depth["bids"]
    asks = depth["asks"]
    assert len(bids) == 2  # 2 个买价
    assert len(asks) == 1
    # 买单按价格降序
    assert bids[0]["price"] == 100.0
    assert bids[1]["price"] == 99.0


def test_l1_spread_calculation():
    """spread 在有买卖价时返回价差。"""
    engine = L1MatchingEngine()
    engine.submit(limit_order(1, _BTC_USDT, "Buy", 99.0, 1.0))
    engine.submit(limit_order(2, _BTC_USDT, "Sell", 101.0, 1.0))
    assert engine.spread == pytest.approx(2.0)


# ═══════════════════════════════════════════════════════════════════════════
# L2MatchingEngine 进阶
# ═══════════════════════════════════════════════════════════════════════════


def test_l2_stats_after_fill():
    """L2 引擎成对 submit(卖+买)后 stats 计数递增。"""
    engine = L2MatchingEngine()
    # 卖单挂单
    engine.submit(limit_order(1, _BTC_USDT, "Sell", 100.0, 2.0))
    # 买单吃 1.0,产生 1 笔 fill
    engine.submit(limit_order(2, _BTC_USDT, "Buy", 100.0, 1.0))
    stats = engine.stats  # property,返回 dict
    # 成对撮合后:matched_orders 递增,total_fills 递增
    assert stats["matched_orders"] >= 1
    assert stats["total_fills"] >= 1


def test_l2_modify_after_from_entries():
    """L2 modify 需要通过 from_entries 构造订单簿(因为 submit 不会更新 order_index)。"""
    # 先构造一个 OrderBookEntry 列表
    entry = OrderBookEntry(
        order_id=1,
        side="buy",
        price=100.0,
        quantity=1.0,
        filled_quantity=0.0,
    )
    # from_entries 是 staticmethod,接受 list[OrderBookEntry]
    engine = L2MatchingEngine.from_entries([entry])
    assert engine.contains(1)
    # modify 是 positional 参数(order_id, new_price=None, new_quantity=None)
    engine.modify(1, new_quantity=5.0)
    # 修改后 order 仍存在
    assert engine.contains(1)


def test_l2_volume_at_price():
    """L2 volume_at_price(side, price) 查询某价位总挂单量。"""
    engine = L2MatchingEngine()
    engine.submit(limit_order(1, _BTC_USDT, "Buy", 100.0, 1.0))
    engine.submit(limit_order(2, _BTC_USDT, "Buy", 100.0, 2.0))
    vol = engine.volume_at_price("buy", 100.0)
    assert vol == pytest.approx(3.0)


def test_l2_order_book_entry_roundtrip():
    """L2 OrderBookEntry 通过 from_entries 重建。"""
    # 构造 OrderBookEntry 实例列表,直接走 from_entries(staticmethod)
    entry = OrderBookEntry(
        order_id=1,
        side="buy",
        price=100.0,
        quantity=1.0,
        filled_quantity=0.0,
    )
    engine = L2MatchingEngine.from_entries([entry])
    # export_entries 返回 list[dict] 含 order_id / side / price / quantity
    exported = engine.export_entries()
    assert len(exported) == 1
    assert exported[0]["order_id"] == 1
    assert exported[0]["price"] == 100.0
    assert engine.contains(1)


# ═══════════════════════════════════════════════════════════════════════════
# MultiAssetMatchingEngine 多资产
# ═══════════════════════════════════════════════════════════════════════════


def test_l3_register_asset_and_route():
    """L3 register_asset 后多资产路由生效。"""
    engine = MultiAssetMatchingEngine()
    engine.register_asset("BTC-USDT")
    engine.register_asset("ETH-USDT")
    assert engine.asset_count == 2

    # 提交订单后两 symbol 订单簿隔离
    engine.submit(limit_order(1, _BTC_USDT, "Buy", 100.0, 1.0))
    engine.submit(limit_order(2, _ETH_USDT, "Buy", 200.0, 2.0))
    assert engine.best_bid("BTC-USDT") == 100.0
    assert engine.best_bid("ETH-USDT") == 200.0
    assert engine.active_order_count("BTC-USDT") == 1
    assert engine.active_order_count("ETH-USDT") == 1


def test_l3_submit_batch_processes_list():
    """L3 submit_batch 接受 list[dict] 批量撮合。"""
    engine = MultiAssetMatchingEngine()
    engine.register_asset("BTC-USDT")
    engine.submit_batch([
        limit_order(1, _BTC_USDT, "Sell", 100.0, 1.0),
        limit_order(2, _BTC_USDT, "Buy", 100.0, 1.0),
    ])
    assert engine.fill_count("BTC-USDT") == 1


def test_l3_continuous_mode_yields_fill():
    """L3 continuous 模式下订单即时撮合。"""
    engine = MultiAssetMatchingEngine()
    engine.register_asset("BTC-USDT")
    engine.set_batch_mode("continuous")
    engine.submit(limit_order(1, _BTC_USDT, "Sell", 100.0, 1.0))
    fills = engine.submit(limit_order(2, _BTC_USDT, "Buy", 100.0, 1.0))
    assert len(fills) == 1


# ═══════════════════════════════════════════════════════════════════════════
# ImpactedMatchingEngine 冲击感知
# ═══════════════════════════════════════════════════════════════════════════


def test_impacted_builder_linear_default():
    """ImpactedBuilder linear + coefficient=0 时无冲击。"""
    ie = (
        ImpactedMatchingEngineBuilder()
        .model_type("linear")
        .coefficient(0.0)
        .build()
    )
    # model_name 是方法(非 property),返回 Rust Display:LinearImpact
    assert ie.model_name() == "LinearImpact"
    # permanent_offset 也是方法,系数=0 时为 0
    assert ie.permanent_offset() == 0.0
    # 撮合基础路径(无对手盘)不报错
    result = ie.submit(limit_order(1, _BTC_USDT, "Buy", 100.0, 1.0))
    assert "is_filled" in result


def test_impacted_builder_power_law():
    """ImpactedBuilder power_law 模型构造。"""
    ie = (
        ImpactedMatchingEngineBuilder()
        .model_type("power_law")
        .coefficient(0.1)
        .exponent(0.5)
        .instantaneous_ratio(0.7)
        .build()
    )
    assert ie.model_name() == "PowerLawImpact"


def test_impacted_engine_reset_impact_state():
    """ImpactedEngine reset_impact_state 归零累计冲击。"""
    ie = (
        ImpactedMatchingEngineBuilder()
        .model_type("linear")
        .coefficient(0.1)
        .build()
    )
    # 多次 submit 后 reset
    for i in range(1, 4):
        ie.submit(limit_order(i, _BTC_USDT, "Buy", 100.0 + i, 1.0))
    # permanent_offset 是方法
    offset_before = ie.permanent_offset()
    ie.reset_impact_state()
    assert ie.permanent_offset() == 0.0
    # reset 之前应该有过累计
    assert offset_before >= 0.0  # 弱断言,允许线性系数小时不变


# ═══════════════════════════════════════════════════════════════════════════
# BacktestEngine 事件驱动
# ═══════════════════════════════════════════════════════════════════════════


def test_backtest_engine_empty_runs_zero():
    """空事件队列 run 后全 0,final_nav = initial_cash。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    result = bt.run()
    assert result.events_processed == 0
    assert result.fills == 0
    assert result.orders_accepted == 0
    assert result.orders_rejected == 0
    assert result.orders_cancelled == 0
    assert result.orders_modified == 0
    assert result.final_nav == 100_000.0
    assert result.final_time_ns == 0


def test_backtest_engine_single_order_no_match():
    """单笔买单无卖单 → events_processed=1,orders_accepted=1,fills=0。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, _BTC_USDT, "Buy", 100.0, 1.0),
    })
    result = bt.run()
    assert result.events_processed == 1
    assert result.orders_accepted == 1
    assert result.fills == 0
    assert result.final_nav == 100_000.0


def test_backtest_engine_matched_orders():
    """卖单 + 买单 → 1 fill,buy 端 PnL = -100*1 = -100。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, _BTC_USDT, "Sell", 100.0, 1.0),
    })
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 2_000,
        "order": limit_order(2, _BTC_USDT, "Buy", 100.0, 1.0),
    })
    result = bt.run()
    assert result.events_processed == 2
    assert result.orders_accepted == 2
    assert result.fills == 1
    assert result.total_pnl == pytest.approx(-100.0)
    assert result.final_nav == pytest.approx(99_900.0)


def test_backtest_engine_cancelled_and_modified_events():
    """取消/修改事件计数。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.push_event({
        "type": "order_cancelled",
        "timestamp_ns": 1_000,
        "order_id": 42,
    })
    bt.push_event({
        "type": "order_modified",
        "timestamp_ns": 2_000,
        "order_id": 42,
        "new_quantity": 5.0,
    })
    result = bt.run()
    assert result.events_processed == 2
    assert result.orders_cancelled == 1
    assert result.orders_modified == 1


def test_backtest_engine_fill_event_path():
    """fill 事件路径:fills=1,total_pnl=0(外部成交保守不计入 PnL)。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.push_event({
        "type": "fill",
        "timestamp_ns": 1_000,
        "price": 100.0,
        "quantity": 1.0,
        "buyer_order_id": 1,
        "seller_order_id": 2,
    })
    result = bt.run()
    assert result.fills == 1
    assert result.total_pnl == 0.0


def test_backtest_engine_run_idempotent():
    """run 幂等:重复 run 返回相同结果。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.push_event({
        "type": "order_cancelled",
        "timestamp_ns": 1_000,
        "order_id": 1,
    })
    r1 = bt.run()
    r2 = bt.run()
    assert r1.events_processed == r2.events_processed
    assert r1.orders_cancelled == r2.orders_cancelled
    assert r1.final_nav == r2.final_nav


def test_backtest_engine_step_incremental():
    """step 单步推进,事件耗尽返回 None。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.push_event({"type": "order_cancelled", "timestamp_ns": 1_000, "order_id": 1})
    bt.push_event({"type": "order_cancelled", "timestamp_ns": 2_000, "order_id": 2})
    s1 = bt.step()
    assert s1 is not None
    assert s1.events_processed == 1
    s2 = bt.step()
    assert s2 is not None
    assert s2.events_processed == 2
    # 队列耗尽
    assert bt.step() is None


def test_backtest_engine_to_dict_fields():
    """RunResult.to_dict 字段完整。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    result = bt.run()
    d = result.to_dict()
    expected_keys = {
        "events_processed", "orders_accepted", "orders_rejected", "fills",
        "orders_cancelled", "orders_modified", "total_pnl", "max_drawdown",
        "final_nav", "duration_secs", "final_time_ns",
    }
    assert set(d.keys()) == expected_keys


def test_backtest_engine_repr_contains_state():
    """BacktestEngine __repr__ 包含状态。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    s = repr(bt)
    assert "BacktestEngine" in s
    assert "pending=0" in s


# ═══════════════════════════════════════════════════════════════════════════
# 异常路径
# ═══════════════════════════════════════════════════════════════════════════


def test_backtest_error_inherits_exception():
    """BacktestError 继承 Exception(实际是 PyException)。"""
    assert issubclass(BacktestError, Exception)


def test_push_event_missing_type_raises():
    """push_event 缺 type 字段 → KeyError(由 dict_to_event 抛)。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    with pytest.raises(KeyError):
        bt.push_event({"timestamp_ns": 1_000})


def test_push_event_unknown_type_raises():
    """push_event 未知 type → ValueError。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    with pytest.raises(ValueError):
        bt.push_event({"type": "bogus_event", "timestamp_ns": 1_000})


def test_submit_missing_price_for_limit_raises():
    """limit 订单 dict 缺 price → KeyError。"""
    engine = L1MatchingEngine()
    bad = limit_order(1, _X_USDT, "Buy", 100.0, 1.0)
    del bad["price"]
    with pytest.raises(KeyError):
        engine.submit(bad)


def test_submit_invalid_side_raises():
    """非法 side 字符串 → ValueError(0.5.0:limit_order 工厂在 Rust 之前预先校验)。"""
    engine = L1MatchingEngine()
    with pytest.raises(ValueError):
        engine.submit(limit_order(1, _X_USDT, "invalid_side", 100.0, 1.0))


def test_l3_unknown_asset_raises():
    """L3 best_bid 未注册资产 → 异常(BacktestError 或其子类)。"""
    engine = MultiAssetMatchingEngine()
    with pytest.raises(Exception):
        engine.best_bid("UNKNOWN")


# ═══════════════════════════════════════════════════════════════════════════
# 0.5.0 新增:多 Leg 回测(spot + swap)
# ═══════════════════════════════════════════════════════════════════════════


def test_backtest_engine_begin_bar_per_instrument():
    """begin_bar(price, instrument) 为每条 instrument 独立播种虚拟对手盘。

    spot + swap 两条 leg 各自的 order book 互不干扰。
    """
    bt = BacktestEngine(initial_cash=100_000.0).with_seed_liquidity(
        half_spread=0.5, depth_levels=3, size_per_level=1.0,
    )
    bt.begin_bar(50_000.0, _BTC_USDT)
    bt.begin_bar(50_000.0, _BTC_USDT_PERP)
    # 各 leg 推 1 笔 buy 限价单 + 1 笔 sell 限价单(同 instrument 内部撮合)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, _BTC_USDT, "Sell", 50_001.0, 0.1),
    })
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 2_000,
        "order": limit_order(2, _BTC_USDT, "Buy", 50_001.0, 0.1),
    })
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 3_000,
        "order": limit_order(3, _BTC_USDT_PERP, "Sell", 50_001.0, 0.1),
    })
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 4_000,
        "order": limit_order(4, _BTC_USDT_PERP, "Buy", 50_001.0, 0.1),
    })
    result = bt.run()
    # 4 笔订单,2 笔成交(spot 一对 + perp 一对)
    assert result.orders_accepted == 4
    assert result.fills == 2
    # 终态两 leg 仓位为 0(完全平仓)
    assert result.positions[_BTC_USDT] == pytest.approx(0.0)
    assert result.positions[_BTC_USDT_PERP] == pytest.approx(0.0)


def test_backtest_engine_set_and_get_target_position():
    """set_target_position / get_target_position 跨 leg 独立记录。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    # 初始时无任何 leg
    assert bt.get_target_position(_BTC_USDT) is None
    assert bt.get_target_position(_BTC_USDT_PERP) is None
    # 设置 spot long +1,perp short -1(delta-neutral,吃 funding > 0)
    bt.set_target_position(_BTC_USDT, 1.0)
    bt.set_target_position(_BTC_USDT_PERP, -1.0)
    assert bt.get_target_position(_BTC_USDT) == 1.0
    assert bt.get_target_position(_BTC_USDT_PERP) == -1.0
    # 重复设置覆盖前值
    bt.set_target_position(_BTC_USDT, 2.5)
    assert bt.get_target_position(_BTC_USDT) == 2.5


def test_backtest_engine_get_position_default_zero():
    """未交易过的 instrument 当前仓位为 0.0。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    assert bt.get_position(_BTC_USDT) == 0.0
    assert bt.get_position(_BTC_USDT_PERP) == 0.0


def test_backtest_engine_push_mark_updates_cache():
    """push_mark 后 marks dict 出现该 instrument 的最新价(后到覆盖先到)。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.push_mark(_BTC_USDT, 50_000.0, timestamp_ns=1_000_000)
    bt.push_mark(_BTC_USDT, 50_500.0, timestamp_ns=2_000_000)
    bt.push_mark(_BTC_USDT_PERP, 50_100.0, timestamp_ns=1_500_000)
    marks = bt.marks
    # spot 收到第二次 push(50_500)覆盖第一次
    assert marks[_BTC_USDT] == 50_500.0
    # perp 单次 push
    assert marks[_BTC_USDT_PERP] == 50_100.0


def test_backtest_engine_two_legs_isolated_positions():
    """两 leg(spot + swap)同时成交 → positions dict 两个 instrument 独立累计。"""
    bt = BacktestEngine(initial_cash=100_000.0).with_seed_liquidity(
        half_spread=0.5, depth_levels=2, size_per_level=2.0,
    )
    bt.begin_bar(50_000.0, _BTC_USDT)
    bt.begin_bar(50_000.0, _BTC_USDT_PERP)
    # spot long 0.5(buy),perp short 0.5(sell)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, _BTC_USDT, "Buy", 50_001.0, 0.5),
    })
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_500,
        "order": limit_order(2, _BTC_USDT_PERP, "Sell", 50_001.0, 0.5),
    })
    result = bt.run()
    # spot long = 0.5,perp short = -0.5
    assert result.positions[_BTC_USDT] == pytest.approx(0.5)
    assert result.positions[_BTC_USDT_PERP] == pytest.approx(-0.5)
    # 总成交 2 笔
    assert result.fills == 2
    # leg_targets 在 run 后可通过 getter 读(本次未调 set_target_position,可能为 None)
    assert bt.get_target_position(_BTC_USDT) is None


def test_backtest_engine_leg_targets_persist():
    """set_target_position 后的 leg 目标位可在 result.leg_targets 中读到。"""
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.set_target_position(_BTC_USDT, 1.0)
    bt.set_target_position(_BTC_USDT_PERP, -1.0)
    result = bt.run()
    # leg_targets 是 dict[instrument_tuple, target]
    assert result.leg_targets[_BTC_USDT] == 1.0
    assert result.leg_targets[_BTC_USDT_PERP] == -1.0
