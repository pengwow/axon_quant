"""axon_quant.risk 顶层 Python API —— thin wrapper 模式(Stage 3)。

约定:
- 核心实现走 ``axon_quant._native.risk``(PyO3 绑定)
- 本模块负责:
  * 重新导出 5 个核心类(DefaultRiskEngine / CircuitBreaker / RiskConfig /
    RiskMetrics / RiskResult / RiskReason)
  * 工厂函数 ``make_order()`` / ``make_portfolio()`` /
    ``make_portfolio_with_positions()`` / ``make_risk_config()`` 让 Python
    用户无需手写 dict 协议字段
  * 类型别名(IDE 友好):``OrderDict`` / ``PortfolioDict``

核心组件:
- 预交易风控主类:``DefaultRiskEngine`` —— ``check_order`` / ``check_portfolio`` /
  ``update_daily_pnl`` / ``reset_daily`` / ``metrics``
- 熔断器:``CircuitBreaker`` —— ``check_and_trigger`` / ``reset`` / ``is_active``
- 配置:``RiskConfig`` —— 8 个风控阈值(单标持仓 / 总敞口 / 单笔价值 / 杠杆 / 回撤 /
  日内亏损 / 集中度 / 熔断冷却)
- 风险指标:``RiskMetrics`` —— NAV / 杠杆 / 回撤 / 日内 PnL / VaR(95) / 集中度
- 结果枚举:``RiskResult`` —— ``Allow`` / ``Reject(reason)`` / ``Warn(msg)``,
  工厂方法 ``allow()`` / ``reject(reason)`` / ``warn(msg)``
- 拒绝原因:``RiskReason`` —— 8 个变体的扁平化标签(``OrderTooLarge`` /
  ``PositionLimitExceeded`` / ...)
- 异常:``RiskError`` —— 继承 builtin ``PyException`` 而非 ``AxonError``,
  避免 ``axon-risk`` 反向依赖 ``axon-python`` 造成 cargo 循环;
  Python 端可走 ``except Exception`` 统一捕获

用法::

    from axon_quant.risk import (
        DefaultRiskEngine, RiskConfig, CircuitBreaker,
        RiskResult, RiskReason, RiskMetrics, RiskError,
        make_order, make_portfolio, make_portfolio_with_positions,
        make_risk_config,
    )

    # 1) 预交易风控
    engine = DefaultRiskEngine(make_risk_config(max_order_value=1000.0))
    order = make_order(id=1, symbol="BTC-USDT", side="Buy",
                       type="limit", price=100.0, quantity=1.0)
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    result = engine.check_order(order, portfolio)
    assert result.is_allow, result.to_dict()

    # 2) 累计日内 PnL 触发熔断
    engine.update_daily_pnl(-1_500.0)
    assert not engine.check_order(order, portfolio).is_allow
    engine.reset_daily()

    # 3) 风险指标
    m = engine.metrics(portfolio)
    print(m["leverage"], m["var_95"])
"""

from __future__ import annotations

from typing import Any, Optional

# 重新导出原生符号(Stage 3 全量)
# 注意:`_native` 是 cdylib 单文件扩展(不是 Python package 目录),
# 所以 `from axon_quant._native.risk import ...` 这种 dot 路径不可用;
# 改用 `from axon_quant._native import risk` 先把子模块对象取出来,
# 再用属性访问取出类(与 `backtest.py` / `data.py` 保持一致)。
from axon_quant._native import risk as _native_risk_module  # noqa: E402

# 显式从子模块对象取值(避免在 top-level 用 `from X import *` 的副作用)
DefaultRiskEngine = _native_risk_module.DefaultRiskEngine
RiskConfig = _native_risk_module.RiskConfig
CircuitBreaker = _native_risk_module.CircuitBreaker
RiskMetrics = _native_risk_module.RiskMetrics
RiskResult = _native_risk_module.RiskResult
RiskReason = _native_risk_module.RiskReason

# 异常:RiskError 继承 builtin `PyException`(避免 cargo 循环,见
# `.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md` §3.1.6)
# 这里**不**继承 `AxonError`(Stage 1 实战发现 cargo 循环不可行)。
# Python 端可走 `except Exception` 或 `except RiskError` 单独捕获。
RiskError = _native_risk_module.RiskError

# 类型别名(IDE 友好)—— 与 Rust 端 dict 协议对齐
# `Order` dict 必填字段:id / symbol / side / type / quantity / tif
# 限价单额外需 price;市价单无需 price
OrderDict = dict[str, Any]
# `Portfolio` dict 必填字段:base_currency / commission_rate
# 可选:cash(`dict[str, float]`) / positions(`dict[str, dict]`)
PortfolioDict = dict[str, Any]
# 订单方向
SideStr = str  # `"Buy" / "Sell"`
# 订单类型
OrderTypeStr = str  # `"market" / "limit"`
# 订单有效期
TifStr = str  # `"GTC" / "IOC" / "FOK" / "GFD" / "FAK"`
# 拒绝原因标签
RiskReasonKindStr = str  # `"OrderTooLarge" / "PositionLimitExceeded" / ...`


__all__ = [
    # 风控主类
    "DefaultRiskEngine",
    # 配置
    "RiskConfig",
    # 熔断器
    "CircuitBreaker",
    # 风险指标
    "RiskMetrics",
    # 结果 / 原因
    "RiskResult",
    "RiskReason",
    # 异常
    "RiskError",
    # 类型别名
    "OrderDict",
    "PortfolioDict",
    "SideStr",
    "OrderTypeStr",
    "TifStr",
    "RiskReasonKindStr",
    # 工厂函数
    "make_order",
    "make_portfolio",
    "make_portfolio_with_positions",
    "make_risk_config",
    "make_circuit_breaker",
]


# ═══════════════════════════════════════════════════════════════════════════
# Order dict 工厂
# ═══════════════════════════════════════════════════════════════════════════


def make_order(
    id: int,
    symbol: str,
    side: SideStr,
    type: OrderTypeStr,
    quantity: float,
    tif: TifStr = "GTC",
    price: Optional[float] = None,
) -> OrderDict:
    """构造风控检查用的 order dict(供 ``DefaultRiskEngine.check_order`` 接收)。

    必填字段:``id`` / ``symbol`` / ``side`` / ``type`` / ``quantity`` / ``tif``
    可选字段:``price``(限价单必填,市价单忽略)

    Args:
        id: 订单 ID(全局唯一,整数)
        symbol: 交易对符号,如 ``"BTC-USDT"``
        side: 订单方向,``"Buy"`` / ``"Sell"``(大小写不敏感,Rust 端统一小写匹配)
        type: 订单类型,``"market"`` / ``"limit"``(风控不支持 ``"stop"`` 等高级类型)
        quantity: 订单数量(浮点)
        tif: 有效期,``"GTC"``(默认) / ``"IOC"`` / ``"FOK"`` / ``"GFD"`` / ``"FAK"``
        price: 限价单价(限价单必填,市价单忽略)

    Returns:
        dict,字段对应 Rust 端 ``Order`` 字段
    """
    if type.lower() == "limit" and price is None:
        raise ValueError("limit order requires 'price'")
    order: OrderDict = {
        "id": int(id),
        "symbol": str(symbol),
        "side": str(side),
        "type": str(type).lower(),
        "quantity": float(quantity),
        "tif": str(tif).upper(),
    }
    if price is not None:
        order["price"] = float(price)
    return order


# ═══════════════════════════════════════════════════════════════════════════
# Portfolio dict 工厂
# ═══════════════════════════════════════════════════════════════════════════


def make_portfolio(
    base_currency: str = "USD",
    commission_rate: float = 0.0,
    cash: Optional[dict[str, float]] = None,
) -> PortfolioDict:
    """构造最简 portfolio dict(只填必填字段,空 cash / 无 positions)。

    Args:
        base_currency: 基础货币,如 ``"USD"`` / ``"USDT"`` / ``"BTC"``
        commission_rate: 佣金率(浮点)
        cash: 各币种余额,例如 ``{"USD": 100_000.0}``

    Returns:
        dict,字段对应 Rust 端 ``Portfolio`` 字段
    """
    p: PortfolioDict = {
        "base_currency": str(base_currency),
        "commission_rate": float(commission_rate),
    }
    if cash is not None:
        p["cash"] = dict(cash)
    return p


def make_portfolio_with_positions(
    base_currency: str,
    cash: dict[str, float],
    positions: dict[str, dict[str, float]],
    commission_rate: float = 0.0,
) -> PortfolioDict:
    """构造含 cash + positions 的 portfolio dict。

    Args:
        base_currency: 基础货币
        cash: 各币种余额
        positions: 持仓字典,key 是 symbol,value 是
            ``{"quantity": float, "avg_cost": float, "market_price"?: float}``
        commission_rate: 佣金率

    Returns:
        dict,字段对应 Rust 端 ``Portfolio`` 字段
    """
    return {
        "base_currency": str(base_currency),
        "commission_rate": float(commission_rate),
        "cash": dict(cash),
        "positions": {
            str(symbol): {
                "quantity": float(pos["quantity"]),
                "avg_cost": float(pos["avg_cost"]),
                **(
                    {"market_price": float(pos["market_price"])}
                    if "market_price" in pos
                    else {}
                ),
            }
            for symbol, pos in positions.items()
        },
    }


# ═══════════════════════════════════════════════════════════════════════════
# RiskConfig / CircuitBreaker 工厂
# ═══════════════════════════════════════════════════════════════════════════


def make_risk_config(
    max_position_per_instrument: float = 100_000.0,
    max_total_exposure: float = 1_000_000.0,
    max_order_value: float = 50_000.0,
    max_leverage: float = 5.0,
    max_drawdown: float = 0.15,
    max_daily_loss: float = 10_000.0,
    max_concentration: float = 0.40,
    circuit_breaker_cooldown_secs: int = 3600,
) -> RiskConfig:
    """构造风控配置(对齐 Rust 端 ``RiskConfig`` 默认值)。

    Args:
        max_position_per_instrument: 单一标的最大持仓
        max_total_exposure: 最大总敞口
        max_order_value: 单笔订单最大价值
        max_leverage: 最大杠杆倍数
        max_drawdown: 最大回撤比例(如 ``0.15`` = 15%)
        max_daily_loss: 日内最大亏损(正值,触发熔断)
        max_concentration: 单一标的占组合最大比例
        circuit_breaker_cooldown_secs: 熔断器冷却秒数

    Returns:
        ``RiskConfig`` 实例(可直接传入 ``DefaultRiskEngine(config)``)
    """
    return RiskConfig(
        max_position_per_instrument=max_position_per_instrument,
        max_total_exposure=max_total_exposure,
        max_order_value=max_order_value,
        max_leverage=max_leverage,
        max_drawdown=max_drawdown,
        max_daily_loss=max_daily_loss,
        max_concentration=max_concentration,
        circuit_breaker_cooldown_secs=circuit_breaker_cooldown_secs,
    )


def make_circuit_breaker(
    daily_loss_limit: float = 10_000.0,
    cooldown_seconds: int = 3600,
) -> CircuitBreaker:
    """构造独立可用的熔断器(不依赖 ``DefaultRiskEngine``)。

    Args:
        daily_loss_limit: 日内亏损阈值(正值,如 ``10000.0``)
        cooldown_seconds: 触发后冷却秒数(冷却期内拒绝新订单)

    Returns:
        ``CircuitBreaker`` 实例
    """
    return CircuitBreaker(
        daily_loss_limit=daily_loss_limit,
        cooldown_seconds=cooldown_seconds,
    )
