"""axon_quant.exchange 顶层 Python API —— thin wrapper 模式(Stage 5)。

约定:
- 核心实现走 ``axon_quant._native.exchange``(PyO3 绑定)
- 本模块负责:
  * 重新导出 9 个核心类 / 异常:
    - 配置:`ExchangeConfig` / `ExchangeId` / `RateLimitConfig` / `ReconnectConfig`
    - 适配器:`BinanceAdapter` / `OkxAdapter`
    - 状态管理:`OrderLifecycleManager`
    - 限流:`TokenBucketRateLimiter`
    - 异常:`ExchangeError`(继承 builtin ``PyException`` 而非 ``AxonError``)
  * 工厂函数 ``binance_testnet_config()`` / ``okx_testnet_config()``
    从环境变量读取 API key,避免硬编码 secret

**安全注意**:
- API key 优先从环境变量读取:
  - Binance:`BINANCE_API_KEY` / `BINANCE_API_SECRET`
  - OKX:`OKX_API_KEY` / `OKX_API_SECRET` / `OKX_PASSPHRASE`
- 默认 ``testnet=True``(testnet URL);生产模式需显式重写 URL
- 永远不要把 ``api_secret`` 写入日志 / ``repr()`` / 异常消息
  (Rust 端 ``__repr__`` 已主动隐藏)

**异步桥**:Rust 端 ``BinanceAdapter`` / ``OkxAdapter`` 所有方法都是
``async``,Python 端用 ``tokio::runtime::Runtime::block_on`` 同步包装
(由 Rust 端 ``#[pymethods]`` 内部完成),Python 端调用方无 asyncio 依赖。

用法::

    import os
    os.environ["BINANCE_API_KEY"] = "your_key"
    os.environ["BINANCE_API_SECRET"] = "your_secret"

    from axon_quant.exchange import (
        BinanceAdapter, ExchangeConfig, ExchangeId,
        binance_testnet_config,
    )

    # 1) 用工厂从 env 读 key(testnet URL 已硬编码)
    cfg = binance_testnet_config()

    # 2) 构造 adapter
    adapter = BinanceAdapter(cfg)

    # 3) 连接 / 下单 / 撤单(同步阻塞)
    adapter.connect()
    oid = adapter.place_order({
        "symbol": "BTCUSDT",
        "side": "buy",
        "type": "limit",
        "quantity": "0.1",
        "price": "50000",
        "tif": "GTC",
    })
    adapter.cancel_order(oid)
    adapter.disconnect()

    # 4) 订单生命周期管理
    from axon_quant.exchange import OrderLifecycleManager
    mgr = OrderLifecycleManager()
    cid = mgr.register_order({
        "symbol": "BTCUSDT",
        "side": "buy",
        "type": "limit",
        "quantity": "0.1",
        "price": "50000",
        "tif": "GTC",
        "exchange": "binance",
    })
    mgr.update_status(cid, {"status": "filled",
                            "filled_qty": "0.1",
                            "avg_price": "50000"})
    print(mgr.active_count(), mgr.history_count())
"""

from __future__ import annotations

import os

# 重新导出原生符号(Stage 5 全量)
# 注意:`_native` 是 cdylib 单文件扩展(不是 Python package 目录),
# 所以 `from axon_quant._native.exchange import ...` 这种 dot 路径不可用;
# 改用 `from axon_quant._native import exchange` 先把子模块对象取出来,
# 再用属性访问取出类(与 `oms.py` / `backtest.py` / `data.py` 保持一致)。
from axon_quant._native import exchange as _native_exchange_module  # noqa: E402

# 显式从子模块对象取值(避免在 top-level 用 `from X import *` 的副作用)
ExchangeConfig = _native_exchange_module.ExchangeConfig
ExchangeId = _native_exchange_module.ExchangeId
RateLimitConfig = _native_exchange_module.RateLimitConfig
ReconnectConfig = _native_exchange_module.ReconnectConfig
BinanceAdapter = _native_exchange_module.BinanceAdapter
OkxAdapter = _native_exchange_module.OkxAdapter
OrderLifecycleManager = _native_exchange_module.OrderLifecycleManager
TokenBucketRateLimiter = _native_exchange_module.TokenBucketRateLimiter

# 异常:ExchangeError 继承 builtin `PyException`(避免 cargo 循环,见
# `.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md` §3.1.6)
# 这里**不**继承 `AxonError`(Stage 1 实战发现 cargo 循环不可行)。
# Python 端可走 `except Exception` 统一捕获。
ExchangeError = _native_exchange_module.ExchangeError

# AxonError 基类(Stage 1 引入,放 data 子模块顶层),
# 这里重新导出方便 Stage 5 用户一处 import。
# 注:ExchangeError **不**继承 AxonError,所以 `except AxonError` 不会
# 捕获 ExchangeError;若想统一处理需 `except (AxonError, ExchangeError)`
# 或直接 `except Exception`。
try:
    from axon_quant import AxonError  # noqa: F401
except ImportError:  # pragma: no cover
    AxonError = None  # type: ignore[assignment]


__all__ = [
    # 配置
    "ExchangeConfig",
    "ExchangeId",
    "RateLimitConfig",
    "ReconnectConfig",
    # 适配器
    "BinanceAdapter",
    "OkxAdapter",
    # 状态管理
    "OrderLifecycleManager",
    "TokenBucketRateLimiter",
    # 异常
    "ExchangeError",
    "AxonError",
    # 工厂函数(env 读 key + testnet 默认)
    "binance_testnet_config",
    "okx_testnet_config",
]


# ═══════════════════════════════════════════════════════════════════════════
# Testnet 配置工厂(从环境变量读 key)
# ═══════════════════════════════════════════════════════════════════════════


def binance_testnet_config() -> ExchangeConfig:
    """构造 Binance testnet 配置,从环境变量读 key。

    优先从以下环境变量读取:
    - ``BINANCE_API_KEY`` —— API key
    - ``BINANCE_API_SECRET`` —— API secret

    testnet URL 已硬编码:
    - REST:``https://testnet.binance.vision``
    - WebSocket:``wss://stream.testnet.binance.vision/ws``

    缺任一环境变量时抛 ``ExchangeError``(继承 ``PyException``)。

    Returns:
        ``ExchangeConfig`` 实例(可传入 ``BinanceAdapter(config)``)

    Examples::

        import os
        os.environ["BINANCE_API_KEY"] = "your_key"
        os.environ["BINANCE_API_SECRET"] = "your_secret"

        from axon_quant.exchange import BinanceAdapter, binance_testnet_config
        adapter = BinanceAdapter(binance_testnet_config())
    """
    api_key = os.environ.get("BINANCE_API_KEY", "")
    api_secret = os.environ.get("BINANCE_API_SECRET", "")
    if not api_key or not api_secret:
        raise ExchangeError(
            "BINANCE_API_KEY / BINANCE_API_SECRET not set in environment"
        )
    return ExchangeConfig(
        exchange_id=ExchangeId.Binance,
        api_key=api_key,
        api_secret=api_secret,
        rest_base_url="https://testnet.binance.vision",
        ws_url="wss://stream.testnet.binance.vision/ws",
        testnet=True,
    )


def okx_testnet_config() -> ExchangeConfig:
    """构造 OKX testnet 配置,从环境变量读 key + passphrase。

    优先从以下环境变量读取:
    - ``OKX_API_KEY`` —— API key
    - ``OKX_API_SECRET`` —— API secret
    - ``OKX_PASSPHRASE`` —— OKX 必须的 passphrase

    testnet URL 已硬编码:
    - REST:``https://www.okx.com``
    - WebSocket:``wss://wspap.okx.com:8443/ws/v5/public?brokerId=9999``

    缺任一环境变量时抛 ``ExchangeError``。

    Returns:
        ``ExchangeConfig`` 实例(可传入 ``OkxAdapter(config)``)

    Examples::

        import os
        os.environ["OKX_API_KEY"] = "your_key"
        os.environ["OKX_API_SECRET"] = "your_secret"
        os.environ["OKX_PASSPHRASE"] = "your_passphrase"

        from axon_quant.exchange import OkxAdapter, okx_testnet_config
        adapter = OkxAdapter(okx_testnet_config())
    """
    api_key = os.environ.get("OKX_API_KEY", "")
    api_secret = os.environ.get("OKX_API_SECRET", "")
    passphrase = os.environ.get("OKX_PASSPHRASE", "")
    if not api_key or not api_secret or not passphrase:
        raise ExchangeError(
            "OKX_API_KEY / OKX_API_SECRET / OKX_PASSPHRASE not set in environment"
        )
    return ExchangeConfig(
        exchange_id=ExchangeId.Okx,
        api_key=api_key,
        api_secret=api_secret,
        passphrase=passphrase,
        rest_base_url="https://www.okx.com",
        ws_url="wss://wspap.okx.com:8443/ws/v5/public?brokerId=9999",
        testnet=True,
    )
