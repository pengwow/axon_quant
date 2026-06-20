"""axon_quant.backtest 顶层 Python API —— thin wrapper 模式。

约定:
- 核心实现走 `axon_quant._native.backtest`(PyO3 绑定)
- 本模块负责:
  * 重新导出 14 个核心类(L1/L2/L3 撮合 + 冲击感知 + 回测主循环 + 异常)
  * `Order` dict 工厂:`limit_order()` / `market_order()` 让 Python
    用户无需手写 dict 协议字段
  * 类型别名(IDE 友好):`OrderDict` / `FillDict` / `SubmitResultDict`

核心组件:
- 撮合引擎:`L1MatchingEngine` / `L2MatchingEngine` /
  `MultiAssetMatchingEngine` / `ImpactedMatchingEngine` /
  `ImpactedMatchingEngineBuilder`
- 订单簿:`OrderBookEntry`(L2 价格-数量条目)
- 多资产:`DarkOrder` / `CrossPair` / `AuctionResult` /
  `ArbitrageOpportunity`
- 回测主循环:`BacktestEngine` / `RunResult` / `RunStats`
- 异常:`BacktestError`(继承 builtin `PyException` 而非 `AxonError`,
  避免 `axon-backtest` 反向依赖 `axon-python` 造成 cargo 循环;
  Python 端可走 `except Exception` 统一捕获)

用法::

    from axon_quant.backtest import (
        L1MatchingEngine, ImpactedMatchingEngine,
        ImpactedMatchingEngineBuilder, BacktestEngine,
        limit_order, market_order, BacktestError,
    )

    # 1) 撮合
    engine = L1MatchingEngine()
    engine.submit(limit_order(1, "BTCUSDT", "Sell", 100.0, 1.0))
    result = engine.submit(limit_order(2, "BTCUSDT", "Buy", 100.0, 1.0))
    assert result["is_filled"] is True

    # 2) 冲击感知
    ie = (
        ImpactedMatchingEngineBuilder()
        .model_type("linear")
        .coefficient(0.1)
        .depth_levels(5)
        .build()
    )
    ie.submit(limit_order(3, "BTCUSDT", "Buy", 100.0, 1.0))

    # 3) 事件驱动回测
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, "BTCUSDT", "Buy", 100.0, 1.0),
    })
    result = bt.run()
    print(result.final_nav, result.fills)
"""

from __future__ import annotations

from typing import Any

# 重新导出原生符号(Stage 2 全量)
# 注意:`_native` 是 cdylib 单文件扩展(不是 Python package 目录),
# 所以 `from axon_quant._native.backtest import ...` 这种 dot 路径不可用;
# 改用 `from axon_quant._native import backtest` 先把子模块对象取出来,
# 再用属性访问取出类(与 `data.py` 保持一致)。
from axon_quant._native import backtest as _native_backtest_module  # noqa: E402

# 显式从子模块对象取值(避免在 top-level 用 `from X import *` 的副作用)
L1MatchingEngine = _native_backtest_module.L1MatchingEngine
L2MatchingEngine = _native_backtest_module.L2MatchingEngine
MultiAssetMatchingEngine = _native_backtest_module.MultiAssetMatchingEngine
OrderBookEntry = _native_backtest_module.OrderBookEntry
ImpactedMatchingEngine = _native_backtest_module.ImpactedMatchingEngine
ImpactedMatchingEngineBuilder = _native_backtest_module.ImpactedMatchingEngineBuilder
BacktestEngine = _native_backtest_module.BacktestEngine
RunResult = _native_backtest_module.RunResult
RunStats = _native_backtest_module.RunStats
DarkOrder = _native_backtest_module.DarkOrder
CrossPair = _native_backtest_module.CrossPair
AuctionResult = _native_backtest_module.AuctionResult
ArbitrageOpportunity = _native_backtest_module.ArbitrageOpportunity

# 异常:BacktestError 继承 builtin `PyException`(避免 cargo 循环,见
# `.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md` §3.1.6)
# 这里**不**继承 `AxonError`(Stage 1 实战发现 cargo 循环不可行)。
# Python 端可走 `except Exception` 或 `except BacktestError` 单独捕获。
BacktestError = _native_backtest_module.BacktestError


# 类型别名(IDE 友好)—— 与 Rust 端 dict 协议对齐
# `Order` dict 必填字段:id / symbol / side / type / quantity / tif
# 限价单额外需 price;市价单无需 price
OrderDict = dict[str, Any]
# `MatchFill` dict 字段:order_id / symbol / side / price / quantity / tif / liquidity_role
FillDict = dict[str, Any]
# `SubmitResult` dict 字段:is_filled / is_partially_filled / remaining_quantity / fills(list[FillDict])
SubmitResultDict = dict[str, Any]
# 多资产批量模式字符串
BatchModeStr = str  # `"continuous" / "auction" / "dark_pool"`
# 订单方向
SideStr = str  # `"Buy" / "Sell"`
# 撮合类型
TifStr = str  # `"GTC" / "IOC" / "FOK"`


__all__ = [
    # 撮合引擎
    "L1MatchingEngine",
    "L2MatchingEngine",
    "MultiAssetMatchingEngine",
    "ImpactedMatchingEngine",
    "ImpactedMatchingEngineBuilder",
    # 订单簿 / 多资产类型
    "OrderBookEntry",
    "DarkOrder",
    "CrossPair",
    "AuctionResult",
    "ArbitrageOpportunity",
    # 回测主循环
    "BacktestEngine",
    "RunResult",
    "RunStats",
    # 异常
    "BacktestError",
    # 类型别名
    "OrderDict",
    "FillDict",
    "SubmitResultDict",
    "BatchModeStr",
    "SideStr",
    "TifStr",
    # 工厂函数
    "limit_order",
    "market_order",
]


# ═══════════════════════════════════════════════════════════════════════════
# Order dict 工厂
# ═══════════════════════════════════════════════════════════════════════════


def limit_order(
    order_id: int,
    symbol: str,
    side: SideStr,
    price: float,
    quantity: float,
    tif: TifStr = "GTC",
) -> OrderDict:
    """构造 limit order dict(供 L1/L2/L3/MultiAsset.submit 接收)。

    必填字段:`id` / `symbol` / `side` / `type` / `price` / `quantity` / `tif`

    Args:
        order_id: 订单 ID(全局唯一,整数)
        symbol: 交易对符号,如 `"BTC-USDT"`
        side: 订单方向,`"Buy"` / `"Sell"`(大小写不敏感,Rust 端统一小写匹配)
        price: 限价单价
        quantity: 订单数量(浮点,内部以 `Quantity::from_f64` 转换)
        tif: 有效期,`"GTC"`(默认) / `"IOC"` / `"FOK"`

    Returns:
        dict,字段对应 Rust 端 `Order` 字段
    """
    return {
        "id": int(order_id),
        "symbol": str(symbol),
        "side": str(side),
        "type": "limit",
        "price": float(price),
        "quantity": float(quantity),
        "tif": str(tif).upper(),
    }


def market_order(
    order_id: int,
    symbol: str,
    side: SideStr,
    quantity: float,
) -> OrderDict:
    """构造 market order dict(供 L1/L2/L3/MultiAsset.submit 接收)。

    市价单**不**需要 `price` 字段(以对手盘最优价即时成交),
    `tif` 强制为 `"IOC"`(立即成交否则取消),与 Rust 端行为一致。

    Args:
        order_id: 订单 ID
        symbol: 交易对符号
        side: 订单方向
        quantity: 订单数量

    Returns:
        dict,字段对应 Rust 端 `Order::Market` 变体
    """
    return {
        "id": int(order_id),
        "symbol": str(symbol),
        "side": str(side),
        "type": "market",
        "quantity": float(quantity),
        "tif": "IOC",
    }
