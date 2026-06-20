"""axon_quant.oms 顶层 Python API —— thin wrapper 模式(Stage 4)。

约定:
- 核心实现走 ``axon_quant._native.oms``(PyO3 绑定)
- 本模块负责:
  * 重新导出 7 个核心类(OrderManager / Order / OrderStatus / Side / OrderType /
    Portfolio / Position)
  * 工厂函数 ``limit_order()`` / ``market_order()`` 让 Python 用户无需手写
    Decimal 字面量
  * 异常 ``OmsError`` 的解释(继承 builtin ``PyException`` 而非 ``AxonError``)

核心组件:
- 主类:``OrderManager`` —— ``submit`` / ``cancel`` / ``update_status`` /
  ``get_order_status`` / ``batch_submit`` / ``add_fill`` / ``active_count`` /
  ``history_count`` / ``snapshot`` / ``snapshot_balance`` / ``snapshot_positions`` /
  ``deposit``
- 订单:``Order`` —— 字段全用 str repr(`quantity` / `price` 是 Decimal 字符串)
- 订单方向:``Side`` —— ``Buy`` / ``Sell``
- 订单类型:``OrderType`` —— ``Limit`` / ``Market`` / ``StopLoss`` / ``StopLimit``
- 状态机:``OrderStatus`` —— ``New`` / ``Submitted`` / ``Acknowledged`` /
  ``PartiallyFilled`` / ``Filled`` / ``Cancelled`` / ``Rejected``
- 组合:``Portfolio`` —— 独立可构造,生产用 ``OrderManager.snapshot_balance``
- 持仓:``Position`` —— ``symbol`` / ``quantity`` / ``avg_price`` / ``realized_pnl`
- 异常:``OmsError`` —— 继承 builtin ``PyException`` 而非 ``AxonError``,
  避免 ``axon-oms`` 反向依赖 ``axon-python`` 造成 cargo 循环

用法::

    from axon_quant.oms import (
        OrderManager, Order, OrderStatus, Side, OrderType,
        Portfolio, Position, OmsError,
        limit_order, market_order,
    )

    # 1) 创建 OMS,存初始资金
    oms = OrderManager()
    oms.deposit("USDT", 100_000)

    # 2) 提交订单(走工厂函数自动处理 Decimal)
    oid = oms.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="k1"))
    oms.update_status(oid, OrderStatus(kind="Acknowledged"))

    # 3) 处理 fill:状态机 + portfolio
    oms.add_fill(
        order_id=oid,
        fill_id="f1",
        symbol="BTC-USDT",
        price=50_000,
        quantity=0.1,  # 正=buy
        fee=0,
    )

    # 4) 查询 portfolio
    snap = oms.snapshot_balance()
    print(snap["cash"]["USDT"], len(snap["positions"]))
"""

from __future__ import annotations

from decimal import Decimal
from typing import Any, Optional

# 重新导出原生符号(Stage 4 全量)
# 注意:`_native` 是 cdylib 单文件扩展(不是 Python package 目录),
# 所以 `from axon_quant._native.oms import ...` 这种 dot 路径不可用;
# 改用 `from axon_quant._native import oms` 先把子模块对象取出来,
# 再用属性访问取出类(与 `backtest.py` / `data.py` / `risk.py` 保持一致)。
from axon_quant._native import oms as _native_oms_module  # noqa: E402

# 显式从子模块对象取值(避免在 top-level 用 `from X import *` 的副作用)
OrderManager = _native_oms_module.OrderManager
Order = _native_oms_module.Order
OrderStatus = _native_oms_module.OrderStatus
Side = _native_oms_module.Side
OrderType = _native_oms_module.OrderType
Portfolio = _native_oms_module.Portfolio
Position = _native_oms_module.Position

# 异常:OmsError 继承 builtin `PyException`(避免 cargo 循环,见
# `.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md` §3.1.7)
# 这里**不**继承 `AxonError`(Stage 1 实战发现 cargo 循环不可行)。
# Python 端可走 `except Exception` 统一捕获。
OmsError = _native_oms_module.OmsError


__all__ = [
    # 主类
    "OrderManager",
    # 类型
    "Order",
    "OrderStatus",
    "Side",
    "OrderType",
    "Portfolio",
    "Position",
    # 异常
    "OmsError",
    # 工厂函数
    "limit_order",
    "market_order",
    "make_order_status",
    # 类型别名
    "OrderDict",
]


# 类型别名(IDE 友好)
# Python 端 `to_dict()` 返回的字段名约定
OrderDict = dict[str, Any]


# ═══════════════════════════════════════════════════════════════════════════
# Order 工厂(避免显式 Decimal import)
# ═══════════════════════════════════════════════════════════════════════════


def limit_order(
    symbol: str,
    side: str,
    quantity: float | str | Decimal,
    price: float | str | Decimal,
    idempotency_key: Optional[str] = None,
) -> Order:
    """构造 limit Order。

    Args:
        symbol: 交易对符号,如 ``"BTC-USDT"``
        side: 订单方向,``"Buy"`` / ``"Sell"``(大小写不敏感)
        quantity: 订单数量(浮点 / 字符串 / ``Decimal``,推荐字符串保精度)
        price: 限价单价(同 quantity)
        idempotency_key: 幂等性键(可选,同一 key 不能重复提交)

    Returns:
        ``Order`` 实例(可传入 ``OrderManager.submit``)
    """
    py_side = Side.Buy if str(side).strip().lower() == "buy" else Side.Sell
    return Order(
        symbol=str(symbol),
        side=py_side,
        order_type=OrderType.Limit,
        quantity=Decimal(str(quantity)),
        price=Decimal(str(price)),
        idempotency_key=idempotency_key,
    )


def market_order(
    symbol: str,
    side: str,
    quantity: float | str | Decimal,
    idempotency_key: Optional[str] = None,
) -> Order:
    """构造 market Order(price 传 0,撮合端按市价吃单)。

    Args:
        symbol: 交易对符号
        side: ``"Buy"`` / ``"Sell"``
        quantity: 订单数量
        idempotency_key: 幂等性键(可选)

    Returns:
        ``Order`` 实例
    """
    py_side = Side.Buy if str(side).strip().lower() == "buy" else Side.Sell
    return Order(
        symbol=str(symbol),
        side=py_side,
        order_type=OrderType.Market,
        quantity=Decimal(str(quantity)),
        price=Decimal("0"),
        idempotency_key=idempotency_key,
    )


# ═══════════════════════════════════════════════════════════════════════════
# OrderStatus 工厂(struct + string tag 模式,不能直接关键字构造)
# ═══════════════════════════════════════════════════════════════════════════


def make_order_status(
    kind: str,
    filled_qty: Optional[float] = None,
    avg_price: Optional[float] = None,
    reason: Optional[str] = None,
) -> OrderStatus:
    """构造 ``OrderStatus`` 实例。

    Rust 端 ``OrderStatus`` 是带数据的 enum,PyO3 0.28 不支持复杂 enum
    variants 暴露,Python 端用 struct + string tag 表达。本函数提供
    dict-style 工厂,自动把数值转 str(走底层 ``OrderStatus.from_dict``)。

    Args:
        kind: 变体名,可选值:
            ``"New"`` / ``"Submitted"`` / ``"Acknowledged"`` /
            ``"PartiallyFilled"`` / ``"Filled"`` / ``"Cancelled"`` / ``"Rejected"``
        filled_qty: ``PartiallyFilled`` / ``Filled`` / ``Cancelled`` 时必填
        avg_price: ``PartiallyFilled`` / ``Filled`` 时必填
        reason: ``Rejected`` 时必填

    Returns:
        ``OrderStatus`` 实例(可传入 ``OrderManager.update_status``)

    Examples::

        oms.update_status(oid, make_order_status("Acknowledged"))
        oms.update_status(oid, make_order_status("Filled", 0.1, 50_000))
        oms.update_status(oid, make_order_status("Rejected", reason="insufficient"))
    """
    d: dict[str, Any] = {"kind": str(kind)}
    if filled_qty is not None:
        d["filled_qty"] = Decimal(str(filled_qty))
    if avg_price is not None:
        d["avg_price"] = Decimal(str(avg_price))
    if reason is not None:
        d["reason"] = str(reason)
    return OrderStatus.from_dict(d)
