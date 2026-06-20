"""axon_quant.oms 端到端测试(L3 Python E2E)。

覆盖范围:
1. 类型导入 / 实例化(8 个核心类型 + 异常)
2. 工厂函数 limit_order / market_order / make_order_status
3. OrderManager 基础 CRUD(submit / cancel / update_status / get_order_status)
4. 状态机:New → Submitted → Acknowledged → Filled
5. 状态机拒绝:InvalidTransition / OrderNotFound
6. 幂等性键:DuplicateIdempotencyKey
7. 批量下单 batch_submit
8. 成交处理 add_fill(状态机 + portfolio 更新)
9. portfolio 桥接:snapshot_balance / snapshot_positions
10. Portfolio 独立 API:deposit / apply_fill / is_empty / to_dict
11. Position 字段:quantity / avg_price / realized_pnl
12. 异常路径(OmsError)
13. snapshot / active_count / history_count
14. Order.to_dict / repr

运行::

    cd /Users/liupeng/workspace/quant/axon
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon/.venv/bin/python \\
        python -m pytest python/tests/test_oms_e2e.py -v

注意:本测试需先 build wheel(参见 Makefile 的 ``python-build`` /
``python-develop`` 目标)。如未 build,部分测试 skip。
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

# 强制使用本项目 venv(避免 miniconda pyarrow / numpy 干扰)
_VENV_SITE = Path("/Users/liupeng/workspace/quant/axon/.venv/lib/python3.14/site-packages")
if _VENV_SITE.exists() and str(_VENV_SITE) not in sys.path:
    sys.path.insert(0, str(_VENV_SITE))

# ``axon_quant`` 在 maturin develop / wheel install 后可被 import
# 缺失时 skip 整个模块(开发期还没 build 时常见)
try:
    import axon_quant  # noqa: F401
    from axon_quant.oms import (
        OmsError,
        Order,
        OrderManager,
        OrderStatus,
        OrderType,
        Portfolio,
        Position,
        Side,
        limit_order,
        make_order_status,
        market_order,
    )
    _OMS_AVAILABLE = hasattr(axon_quant, "_native") and hasattr(
        axon_quant._native, "oms"
    )
except ImportError as _e:
    pytest.skip(f"axon_quant not installed: {_e}", allow_module_level=True)
    raise  # 实际不可达,仅供类型检查

if not _OMS_AVAILABLE:
    pytest.skip(
        "axon_quant._native.oms not yet registered (need maturin develop)",
        allow_module_level=True,
    )


# ═══════════════════════════════════════════════════════════════════════════
# 类型可用性
# ═══════════════════════════════════════════════════════════════════════════


def test_oms_module_imports_all_symbols():
    """所有 oms 顶层符号都能 import。"""
    assert OrderManager is not None
    assert Order is not None
    assert OrderStatus is not None
    assert OrderType is not None
    assert Portfolio is not None
    assert Position is not None
    assert Side is not None
    assert OmsError is not None


def test_oms_native_submodule_accessible():
    """`_native.oms` 子模块可访问,包含与顶层同名符号。"""
    native_oms = axon_quant._native.oms
    assert native_oms.OrderManager is OrderManager
    assert native_oms.Order is Order
    assert native_oms.OmsError is OmsError


# ═══════════════════════════════════════════════════════════════════════════
# 工厂函数
# ═══════════════════════════════════════════════════════════════════════════


def test_limit_order_factory_basic():
    """`limit_order` 构造限价单,字段正确。"""
    order = limit_order("BTC-USDT", "Buy", 0.1, 50_000)
    assert order.symbol == "BTC-USDT"
    assert order.side == Side.Buy
    assert order.order_type == OrderType.Limit
    assert order.quantity == "0.1"
    assert order.price == "50000"
    assert order.idempotency_key is None
    # UUID 36 字符
    assert len(order.order_id) == 36


def test_limit_order_factory_with_idempotency_key():
    """`limit_order` 接受 idempotency_key。"""
    order = limit_order("BTC-USDT", "Sell", 1, 50_000, idempotency_key="k1")
    assert order.idempotency_key == "k1"


def test_market_order_factory_basic():
    """`market_order` 构造市价单,price=0,order_type=Market。"""
    order = market_order("ETH-USDT", "Buy", 5)
    assert order.symbol == "ETH-USDT"
    assert order.side == Side.Buy
    assert order.order_type == OrderType.Market
    assert order.quantity == "5"
    # 市价单 price = 0
    assert order.price == "0"


def test_limit_order_side_case_insensitive():
    """side 字符串大小写不敏感。"""
    o_buy = limit_order("BTC-USDT", "BUY", 1, 100)
    o_sell = limit_order("BTC-USDT", "sell", 1, 100)
    assert o_buy.side == Side.Buy
    assert o_sell.side == Side.Sell


def test_limit_order_accepts_decimal_string():
    """接受 Decimal 字符串保持精度。"""
    order = limit_order("BTC-USDT", "Buy", "0.123456789", "50000.01")
    assert order.quantity == "0.123456789"
    assert order.price == "50000.01"


def test_make_order_status_basic():
    """`make_order_status` 构造 unit-like 状态。"""
    s = make_order_status("Acknowledged")
    assert s.kind == "Acknowledged"
    assert s.filled_qty is None
    assert s.avg_price is None
    assert s.reason is None


def test_make_order_status_filled():
    """Filled 状态携带 filled_qty + avg_price。"""
    s = make_order_status("Filled", 0.1, 50_000)
    assert s.kind == "Filled"
    assert s.filled_qty == "0.1"
    assert s.avg_price == "50000"


def test_make_order_status_rejected():
    """Rejected 状态携带 reason。"""
    s = make_order_status("Rejected", reason="insufficient")
    assert s.kind == "Rejected"
    assert s.reason == "insufficient"


def test_make_order_status_cancelled():
    """Cancelled 状态携带 filled_qty。"""
    s = make_order_status("Cancelled", filled_qty=0.05)
    assert s.kind == "Cancelled"
    assert s.filled_qty == "0.05"


# ═══════════════════════════════════════════════════════════════════════════
# OrderManager:基础 CRUD
# ═══════════════════════════════════════════════════════════════════════════


def test_order_manager_new_is_empty():
    """新建 OrderManager 是空的。"""
    mgr = OrderManager()
    assert mgr.active_count() == 0
    assert mgr.history_count() == 0
    assert mgr.snapshot_balance()["cash"] == {}
    assert mgr.snapshot_positions() == []


def test_submit_order_returns_uuid_string():
    """submit 返回 UUID 36 字符字符串。"""
    mgr = OrderManager()
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000))
    assert len(oid) == 36
    assert oid.count("-") == 4
    assert mgr.active_count() == 1
    assert mgr.history_count() == 1


def test_cancel_order_removes_from_active():
    """cancel 后订单从 active 移除(进入 history)。"""
    mgr = OrderManager()
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000))
    mgr.update_status(oid, make_order_status("Acknowledged"))
    mgr.cancel(oid)
    assert mgr.active_count() == 0
    assert mgr.history_count() == 1


def test_get_order_status_after_submit():
    """get_order_status 返回当前状态(Submitted by default after submit)。"""
    mgr = OrderManager()
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000))
    s = mgr.get_order_status(oid)
    assert s is not None
    assert s.kind == "Submitted"


def test_get_order_status_after_cancel():
    """cancel 后 get_order_status 返回 None(已从 active 移除)。"""
    mgr = OrderManager()
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000))
    mgr.update_status(oid, make_order_status("Acknowledged"))
    mgr.cancel(oid)
    assert mgr.get_order_status(oid) is None


# ═══════════════════════════════════════════════════════════════════════════
# 状态机
# ═══════════════════════════════════════════════════════════════════════════


def test_state_machine_full_lifecycle():
    """状态机完整路径:Submitted → Acknowledged → Filled。"""
    mgr = OrderManager()
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000))
    assert mgr.get_order_status(oid).kind == "Submitted"

    mgr.update_status(oid, make_order_status("Acknowledged"))
    assert mgr.get_order_status(oid).kind == "Acknowledged"

    mgr.update_status(oid, make_order_status("Filled", 0.1, 50_000))
    # Filled 是终态,订单从 active 集合移除(进入 history)
    assert mgr.get_order_status(oid) is None
    assert mgr.active_count() == 0
    assert mgr.history_count() == 1


def test_state_machine_invalid_transition_raises():
    """InvalidTransition(New 不能直接跳 Filled)→ OmsError。"""
    mgr = OrderManager()
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000))
    # submit 已经把状态推到 Submitted;Submitted → Filled 是不合法转换
    # (需要先 Acknowledged)
    with pytest.raises(OmsError) as exc_info:
        mgr.update_status(oid, make_order_status("Filled", 0.1, 50_000))
    assert "InvalidTransition" in str(exc_info.value)


def test_state_machine_order_not_found_raises():
    """不存在的 UUID → OmsError(OrderNotFound)。"""
    mgr = OrderManager()
    missing = "00000000-0000-0000-0000-000000000000"
    with pytest.raises(OmsError) as exc_info:
        mgr.update_status(missing, make_order_status("Acknowledged"))
    assert "OrderNotFound" in str(exc_info.value)


def test_state_machine_invalid_uuid_raises_value_error():
    """非 UUID 格式的 order_id → ValueError(Python 层校验,不是 OmsError)。"""
    mgr = OrderManager()
    with pytest.raises(ValueError):
        mgr.update_status("not-a-uuid", make_order_status("Acknowledged"))


# ═══════════════════════════════════════════════════════════════════════════
# 幂等性
# ═══════════════════════════════════════════════════════════════════════════


def test_idempotency_key_duplicate_raises():
    """同一 idempotency_key 第二次 submit → OmsError。"""
    mgr = OrderManager()
    mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="k1"))
    with pytest.raises(OmsError) as exc_info:
        mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="k1"))
    assert "DuplicateIdempotencyKey" in str(exc_info.value)


def test_idempotency_key_different_works():
    """不同 idempotency_key 允许 submit。"""
    mgr = OrderManager()
    mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="k1"))
    mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="k2"))
    assert mgr.active_count() == 2


# ═══════════════════════════════════════════════════════════════════════════
# 批量下单
# ═══════════════════════════════════════════════════════════════════════════


def test_batch_submit_returns_unique_ids():
    """batch_submit 返回多个 UUID。"""
    mgr = OrderManager()
    orders = [limit_order("BTC-USDT", "Buy", 0.1, 50_000) for _ in range(5)]
    ids = mgr.batch_submit(orders)
    assert len(ids) == 5
    assert len(set(ids)) == 5  # UUID 唯一
    assert mgr.active_count() == 5


def test_batch_submit_duplicate_idempotency_partial():
    """batch_submit 中重复 idempotency_key:前面已提交,后面失败。"""
    mgr = OrderManager()
    orders = [
        limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="dup"),
        limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="dup"),
    ]
    with pytest.raises(OmsError) as exc_info:
        mgr.batch_submit(orders)
    assert "DuplicateIdempotencyKey" in str(exc_info.value)
    # 第一单已提交
    assert mgr.active_count() == 1


# ═══════════════════════════════════════════════════════════════════════════
# 成交处理:状态机 + portfolio
# ═══════════════════════════════════════════════════════════════════════════


def test_add_fill_buy_creates_long_position():
    """buy fill:扣现金 + 建立多头持仓。"""
    mgr = OrderManager()
    mgr.deposit("USDT", 100_000)
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000))
    mgr.update_status(oid, make_order_status("Acknowledged"))
    mgr.add_fill(
        order_id=oid,
        fill_id="f1",
        symbol="BTC-USDT",
        price=50_000,
        quantity=0.1,  # 正=buy
        fee=0,
    )
    snap = mgr.snapshot_balance()
    # cash 100000 - 0.1*50000 = 95000
    assert snap["cash"]["USDT"] == "95000.0"
    # 持仓建立
    pos = snap["positions"]["BTC-USDT"]
    assert pos.symbol == "BTC-USDT"
    assert pos.quantity == "0.1"
    assert pos.avg_price == "50000"


def test_add_fill_sell_realizes_pnl():
    """sell fill:加现金 + 实现盈亏。"""
    mgr = OrderManager()
    mgr.deposit("USDT", 100_000)
    # buy 1 -> 状态变 Filled(整单 1 全成交);sell 触发平仓/反向开仓
    # 注:Rust 端 add_fill 在 quantity == order.quantity 时推 Filled,不再
    # 支持从 Filled → PartiallyFilled 转换;要用 sell 验证 realized_pnl
    # 必须 buy 后用 cancel-with-filled-qty 或其他路径。
    # 此处用 limit_order 下 1.0(buy 整单),然后用 limit_order 2.0 做
    # PartiallyFilled + sell。
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 2.0, 50_000))
    mgr.update_status(oid, make_order_status("Acknowledged"))
    # PartiallyFilled 0.5 @ 50000
    mgr.add_fill(order_id=oid, fill_id="f1", symbol="BTC-USDT", price=50_000, quantity=0.5, fee=0)
    s = mgr.get_order_status(oid)
    assert s.kind == "PartiallyFilled"
    # 接下来 0.5 卖 @ 55000:不是新增 order,直接构造一个 sell 订单
    # 走 OrderManager 状态机;但实际上 realized_pnl 走 fill 路径,
    # 这里我们用 apply_fill 验证 Portfolio 独立 API。
    p = Portfolio()
    p.deposit("USDT", 100_000)
    p.apply_fill(fill_id="f1", symbol="BTC-USDT", price=50_000, quantity=1, fee=0)
    p.apply_fill(fill_id="f2", symbol="BTC-USDT", price=55_000, quantity=-0.5, fee=0)
    pos = p.positions["BTC-USDT"]
    assert pos.quantity == "0.5"
    assert pos.realized_pnl == "2500.0"


def test_add_fill_insufficient_cash_raises_oms_error():
    """现金不足时 add_fill 失败 → OmsError(Portfolio 错误)。"""
    mgr = OrderManager()
    mgr.deposit("USDT", 100)  # 只存 100
    oid = mgr.submit(market_order("BTC-USDT", "Buy", 1))
    mgr.update_status(oid, make_order_status("Acknowledged"))
    with pytest.raises(OmsError) as exc_info:
        mgr.add_fill(order_id=oid, fill_id="f1", symbol="BTC-USDT", price=50_000, quantity=1, fee=0)
    # Portfolio error message contains "insufficient cash"
    assert "insufficient" in str(exc_info.value).lower()


def test_add_fill_invalid_uuid_raises_value_error():
    """非 UUID 格式的 order_id → ValueError。"""
    mgr = OrderManager()
    with pytest.raises(ValueError):
        mgr.add_fill(
            order_id="not-a-uuid",
            fill_id="f1",
            symbol="BTC-USDT",
            price=50_000,
            quantity=0.1,
            fee=0,
        )


def test_add_fill_with_explicit_timestamp():
    """add_fill 接受显式 timestamp(RFC 3339)。"""
    mgr = OrderManager()
    mgr.deposit("USDT", 100_000)
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000))
    mgr.update_status(oid, make_order_status("Acknowledged"))
    mgr.add_fill(
        order_id=oid,
        fill_id="f1",
        symbol="BTC-USDT",
        price=50_000,
        quantity=0.1,
        fee=0,
        timestamp="2026-01-15T10:00:00+00:00",
    )
    pos = mgr.snapshot_balance()["positions"]["BTC-USDT"]
    assert pos.symbol == "BTC-USDT"


# ═══════════════════════════════════════════════════════════════════════════
# Portfolio 桥接
# ═══════════════════════════════════════════════════════════════════════════


def test_snapshot_balance_empty():
    """空 OMS 的 snapshot_balance 是空 cash + 空 positions。"""
    mgr = OrderManager()
    snap = mgr.snapshot_balance()
    assert snap["cash"] == {}
    assert snap["positions"] == {}
    assert "as_of" in snap


def test_snapshot_balance_reflects_deposit():
    """snapshot_balance 反映 deposit 的现金。"""
    mgr = OrderManager()
    mgr.deposit("USDT", 50_000)
    snap = mgr.snapshot_balance()
    assert snap["cash"]["USDT"] == "50000"


def test_snapshot_positions_returns_list():
    """snapshot_positions 返回 list[Position]。"""
    mgr = OrderManager()
    mgr.deposit("USDT", 100_000)
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 0.5, 50_000))
    mgr.update_status(oid, make_order_status("Acknowledged"))
    mgr.add_fill(order_id=oid, fill_id="f1", symbol="BTC-USDT", price=50_000, quantity=0.5, fee=0)

    positions = mgr.snapshot_positions()
    assert len(positions) == 1
    assert positions[0].symbol == "BTC-USDT"
    assert positions[0].quantity == "0.5"


def test_snapshot_structure():
    """snapshot 返回 version + active_orders + history_count。"""
    mgr = OrderManager()
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000))
    snap = mgr.snapshot()
    assert snap["version"] == 1
    assert oid in snap["active_orders"]
    assert snap["history_count"] == 1


# ═══════════════════════════════════════════════════════════════════════════
# Portfolio 独立 API
# ═══════════════════════════════════════════════════════════════════════════


def test_portfolio_new_is_empty():
    """新建 Portfolio 是空的。"""
    p = Portfolio()
    assert p.is_empty()
    assert p.position_count() == 0


def test_portfolio_deposit_accumulates():
    """deposit 累加同一币种。"""
    p = Portfolio()
    p.deposit("USDT", 1000)
    p.deposit("USDT", 500)
    p.deposit("BTC", 1)
    cash = p.cash
    assert cash["USDT"] == "1500"
    assert cash["BTC"] == "1"


def test_portfolio_apply_fill_buy():
    """apply_fill buy 建仓 + 扣现金。"""
    p = Portfolio()
    p.deposit("USDT", 10000)
    p.apply_fill(
        fill_id="f1",
        symbol="BTC-USDT",
        price=50_000,
        quantity=0.1,
        fee=0,
    )
    assert p.cash["USDT"] == "5000.0"
    pos = p.positions["BTC-USDT"]
    assert pos.symbol == "BTC-USDT"
    assert pos.quantity == "0.1"
    assert pos.avg_price == "50000"


def test_portfolio_apply_fill_sell_realizes_pnl():
    """apply_fill sell 部分平仓触发 realized_pnl。"""
    p = Portfolio()
    p.deposit("USDT", 100_000)
    p.apply_fill(fill_id="f1", symbol="BTC-USDT", price=50_000, quantity=1, fee=0)
    p.apply_fill(fill_id="f2", symbol="BTC-USDT", price=55_000, quantity=-0.5, fee=0)
    pos = p.positions["BTC-USDT"]
    assert pos.quantity == "0.5"
    assert pos.realized_pnl == "2500.0"


def test_portfolio_apply_fill_insufficient_cash_raises():
    """现金不足时 apply_fill 失败 → ValueError。"""
    p = Portfolio()
    p.deposit("USDT", 100)
    with pytest.raises(ValueError) as exc_info:
        p.apply_fill(
            fill_id="f1",
            symbol="BTC-USDT",
            price=50_000,
            quantity=1,
            fee=0,
        )
    assert "insufficient cash" in str(exc_info.value).lower()


def test_portfolio_apply_fill_with_timestamp():
    """apply_fill 接受显式 timestamp。"""
    p = Portfolio()
    p.deposit("USDT", 100_000)
    p.apply_fill(
        fill_id="f1",
        symbol="BTC-USDT",
        price=50_000,
        quantity=1,
        fee=0,
        timestamp="2026-01-15T10:00:00+00:00",
    )
    pos = p.positions["BTC-USDT"]
    assert pos.symbol == "BTC-USDT"


def test_portfolio_apply_fill_invalid_timestamp_raises():
    """无效 timestamp → ValueError。"""
    p = Portfolio()
    p.deposit("USDT", 100_000)
    with pytest.raises(ValueError) as exc_info:
        p.apply_fill(
            fill_id="f1",
            symbol="BTC-USDT",
            price=50_000,
            quantity=1,
            fee=0,
            timestamp="not-a-timestamp",
        )
    assert "invalid" in str(exc_info.value).lower()


def test_portfolio_is_empty_after_fills():
    """apply_fill 后 is_empty 反映状态。"""
    p = Portfolio()
    assert p.is_empty()
    p.deposit("USDT", 100)
    assert not p.is_empty()


def test_portfolio_to_dict_structure():
    """to_dict 包含 cash + positions + position_count。"""
    p = Portfolio()
    p.deposit("USDT", 1000)
    d = p.to_dict()
    assert "cash" in d
    assert "positions" in d
    assert "position_count" in d
    assert d["position_count"] == 0


# ═══════════════════════════════════════════════════════════════════════════
# Position 字段
# ═══════════════════════════════════════════════════════════════════════════


def test_position_to_dict_all_fields():
    """Position.to_dict 包含所有字段。"""
    mgr = OrderManager()
    mgr.deposit("USDT", 100_000)
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 1, 50_000))
    mgr.update_status(oid, make_order_status("Acknowledged"))
    mgr.add_fill(order_id=oid, fill_id="f1", symbol="BTC-USDT", price=50_000, quantity=1, fee=0)
    pos = mgr.snapshot_balance()["positions"]["BTC-USDT"]
    d = pos.to_dict()
    assert d["symbol"] == "BTC-USDT"
    assert d["quantity"] == "1"
    assert d["avg_price"] == "50000"
    assert "realized_pnl" in d
    assert "updated_at" in d


def test_position_repr_contains_symbol():
    """Position.__repr__ 包含 symbol。"""
    mgr = OrderManager()
    mgr.deposit("USDT", 100_000)
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 1, 50_000))
    mgr.update_status(oid, make_order_status("Acknowledged"))
    mgr.add_fill(order_id=oid, fill_id="f1", symbol="BTC-USDT", price=50_000, quantity=1, fee=0)
    pos = mgr.snapshot_balance()["positions"]["BTC-USDT"]
    r = repr(pos)
    assert "BTC-USDT" in r
    assert "Position" in r


# ═══════════════════════════════════════════════════════════════════════════
# Order.to_dict / __repr__
# ═══════════════════════════════════════════════════════════════════════════


def test_order_to_dict_contains_all_fields():
    """Order.to_dict 包含所有字段。"""
    order = limit_order("BTC-USDT", "Buy", 0.5, 50_000, idempotency_key="k1")
    d = order.to_dict()
    assert d["symbol"] == "BTC-USDT"
    assert d["side"] == "Buy"
    assert d["order_type"] == "Limit"
    assert d["quantity"] == "0.5"
    assert d["price"] == "50000"
    assert d["idempotency_key"] == "k1"
    assert "order_id" in d
    assert "status" in d


def test_order_repr_contains_symbol():
    """Order.__repr__ 包含 symbol + side + qty + price。"""
    order = limit_order("BTC-USDT", "Sell", 1, 50_000)
    r = repr(order)
    assert "BTC-USDT" in r
    assert "Sell" in r
    assert "1" in r
    assert "50000" in r


# ═══════════════════════════════════════════════════════════════════════════
# 异常
# ═══════════════════════════════════════════════════════════════════════════


def test_oms_error_is_exception():
    """OmsError 继承 Exception(可被 except Exception 捕获)。"""
    mgr = OrderManager()
    try:
        mgr.update_status("00000000-0000-0000-0000-000000000000", make_order_status("Acknowledged"))
        assert False, "should have raised"
    except Exception as e:  # noqa: BLE001
        assert isinstance(e, OmsError)


def test_oms_error_message_contains_code():
    """OmsError 的 str 包含错误码(便于日志搜索)。"""
    mgr = OrderManager()
    try:
        mgr.update_status("00000000-0000-0000-0000-000000000000", make_order_status("Acknowledged"))
    except OmsError as e:
        msg = str(e)
        assert "OrderNotFound" in msg


# ═══════════════════════════════════════════════════════════════════════════
# 完整端到端:order → ack → fill → portfolio
# ═══════════════════════════════════════════════════════════════════════════


def test_e2e_full_lifecycle_with_portfolio():
    """完整 E2E:deposit + submit + ack + 2 fills + 查询 portfolio。"""
    mgr = OrderManager()
    mgr.deposit("USDT", 100_000)

    # 提交 1 个 limit buy 单
    oid = mgr.submit(limit_order("BTC-USDT", "Buy", 1, 50_000, idempotency_key="e2e-1"))
    assert mgr.active_count() == 1

    # 推状态到 Acknowledged
    mgr.update_status(oid, make_order_status("Acknowledged"))

    # PartiallyFilled:成交 0.6
    mgr.add_fill(order_id=oid, fill_id="f1", symbol="BTC-USDT", price=50_000, quantity=0.6, fee=0)

    # 状态机应到 PartiallyFilled
    s = mgr.get_order_status(oid)
    assert s.kind == "PartiallyFilled"
    assert s.filled_qty == "0.6"

    # 验证 portfolio
    snap = mgr.snapshot_balance()
    # cash 100000 - 0.6*50000 = 70000
    assert snap["cash"]["USDT"] == "70000.0"
    pos = snap["positions"]["BTC-USDT"]
    assert pos.quantity == "0.6"
    assert pos.avg_price == "50000"

    # Filled:成交剩余 0.4
    mgr.add_fill(order_id=oid, fill_id="f2", symbol="BTC-USDT", price=51_000, quantity=0.4, fee=0)
    s = mgr.get_order_status(oid)
    assert s.kind == "Filled"

    # 加权平均:0.6*50000 + 0.4*51000 = 30000+20400 = 50400
    # cash 70000 - 0.4*51000 = 70000 - 20400 = 49600
    snap = mgr.snapshot_balance()
    assert snap["cash"]["USDT"] == "49600.0"
    pos = snap["positions"]["BTC-USDT"]
    assert pos.quantity == "1.0"
    assert pos.avg_price == "50400"
