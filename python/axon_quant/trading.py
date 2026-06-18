"""AXON Trading 顶层 Python API

将 Rust 端 ``axon-llm`` 的 trading PyO3 绑定封装为更友好的 Python 接口。

设计原则
=========

1. **类型别名为主**:直接 re-export Rust 端的 ``RiskLimits`` / ``MockTradingBackend``
   / 4 个 Tool / ``TradingMetrics``,不引入额外 dataclass 包装。
2. **dict 透传**:Tool 的 ``execute`` 接受 Python ``dict``,内部转 Rust 结构体,
   返回值是 ``dict``(便于 LLM 消费 JSON)。
3. **Mock 后端优先**:Stage K 只暴露 ``MockTradingBackend``(具体类),不暴露
   ``TradingBackend`` trait object。真实交易所(Exchange/OMS/Backtest)按需
   在 Python 侧自实现或单独开 stage 暴露。

典型用法
========

最小化 mock 闭环::

    from axon_quant.trading import (
        RiskLimits, MockTradingBackend,
        PlaceOrderTool, QueryPortfolioTool,
    )

    backend = MockTradingBackend()
    risk = RiskLimits(
        max_order_notional=100.0,
        max_daily_orders=20,
        allowed_symbols=["BTC-USDT"],
    )
    place = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk)
    query = QueryPortfolioTool(backend=backend)

    ack = place.execute({
        "symbol": "BTC-USDT",
        "side": "Buy",
        "quantity": 0.1,
        "price": 50000.0,
    })
    print(ack["status"])  # "DryRun"

    portfolio = query.execute()
    print(portfolio)

撤单 / 改单::

    from axon_quant.trading import CancelOrderTool, ReplaceOrderTool

    cancel = CancelOrderTool(backend=backend, risk=risk)
    cancel.execute({"order_id": "MOCK-1"})

    replace = ReplaceOrderTool(backend=backend, risk=risk)
    replace.execute({
        "order_id": "MOCK-1",
        "new_req": {
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.2,
            "price": 51000.0,
        },
    })

指标埋点::

    from axon_quant.trading import TradingMetrics

    metrics = TradingMetrics()
    place = PlaceOrderTool(backend=backend, mode="direct", risk=risk, metrics=metrics)
    # ... 执行若干操作 ...
    samples = metrics.snapshot()  # list of {name, kind, value, labels}
"""

from __future__ import annotations

# 从原生 Rust 扩展导入底层类
# 注意:`_native` 是 cdylib 单文件扩展(不是 Python package 目录),
# 所以 `from ._native.trading import ...` 这种 dot 路径不可用;
# 改用 `from axon_quant._native import trading` 先把子模块对象取出来,
# 再用属性访问取出类。
from axon_quant._native import trading as _native_trading_module  # noqa: E402

# 拉出 Rust 端 7 个核心类(在 Python 端用类型别名对外暴露)
_RustRiskLimits = _native_trading_module.RiskLimits
_RustMockTradingBackend = _native_trading_module.MockTradingBackend
_RustPlaceOrderTool = _native_trading_module.PlaceOrderTool
_RustQueryPortfolioTool = _native_trading_module.QueryPortfolioTool
_RustCancelOrderTool = _native_trading_module.CancelOrderTool
_RustReplaceOrderTool = _native_trading_module.ReplaceOrderTool
_RustTradingMetrics = _native_trading_module.TradingMetrics

# 类型别名:Python 用户直接用 ``RiskLimits`` / ``PlaceOrderTool`` 等,
# 不必关心 Rust 内部命名(Rust 内部用 ``Py`` 前缀做隔离)
RiskLimits = _RustRiskLimits
MockTradingBackend = _RustMockTradingBackend
PlaceOrderTool = _RustPlaceOrderTool
QueryPortfolioTool = _RustQueryPortfolioTool
CancelOrderTool = _RustCancelOrderTool
ReplaceOrderTool = _RustReplaceOrderTool
TradingMetrics = _RustTradingMetrics

__all__ = [
    "RiskLimits",
    "MockTradingBackend",
    "PlaceOrderTool",
    "QueryPortfolioTool",
    "CancelOrderTool",
    "ReplaceOrderTool",
    "TradingMetrics",
]
