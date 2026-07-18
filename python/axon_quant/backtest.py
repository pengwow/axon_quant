"""axon_quant.backtest 顶层 Python API —— thin wrapper 模式。

约定:
- 核心实现走 `axon_quant._native.backtest`(PyO3 绑定)
- 本模块负责:
  * 重新导出 14 个核心类(L1/L2/L3 撮合 + 冲击感知 + 回测主循环 + 异常)
  * `Order` dict 工厂:`limit_order()` / `market_order()` 让 Python
    用户无需手写 dict 协议字段
  * `Instrument` dict 工厂:`spot_instrument()` / `swap_instrument()`
    让 Python 用户无需手写 0.5.0 引入的 `instrument` 子 dict
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
        limit_order, market_order, spot_instrument, swap_instrument,
        BacktestError,
    )

    # 1) 撮合(0.5.0 起:`symbol` 字符串被 `instrument` dict 取代)
    btc_spot = spot_instrument("BTC", "USDT")
    engine = L1MatchingEngine()
    engine.submit(limit_order(1, btc_spot, "Sell", 100.0, 1.0))
    result = engine.submit(limit_order(2, btc_spot, "Buy", 100.0, 1.0))
    assert result["is_filled"] is True

    # 2) Delta-neutral 两腿套利(spot + swap)
    spot = spot_instrument("BTC", "USDT")
    perp = swap_instrument("BTC", "USDT", settle="usd_margin", contract_size=1.0)
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.begin_bar(50_000.0, spot)
    bt.begin_bar(50_000.0, perp)
    # 设置腿目标位:spot long +1,perp short -1(吃 funding > 0)
    bt.set_target_position(spot, 1.0)
    bt.set_target_position(perp, -1.0)
    result = bt.run()

    # 3) 事件驱动回测
    bt2 = BacktestEngine(initial_cash=100_000.0)
    bt2.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, spot, "Buy", 100.0, 1.0),
    })
    result = bt2.run()
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
# 0.5.0 起:Order dict 必填字段:id / instrument(dict) / side / type / quantity / tif
# 限价单额外需 price;市价单无需 price
OrderDict = dict[str, Any]
# Instrument dict 子结构(供 IDE 提示):
# - spot: {"kind": "spot", "base": "BTC", "quote": "USDT"}
# - swap: {"kind": "swap", "base": "BTC", "quote": "USDT",
#          "settle": "usd_margin" | "coin_margin", "contract_size": 1.0}
InstrumentDict = dict[str, Any]
# `MatchFill` dict 字段:fill_id / taker_order_id / maker_order_id / price / quantity /
# taker_side / timestamp_ns
FillDict = dict[str, Any]
# `SubmitResult` dict 字段:is_filled / is_partially_filled / remaining_quantity / fills(list[FillDict])
SubmitResultDict = dict[str, Any]
# 多资产批量模式字符串
BatchModeStr = str  # `"continuous" / "auction" / "dark_pool"`
# 订单方向
SideStr = str  # `"Buy" / "Sell"`
# 撮合类型
TifStr = str  # `"GTC" / "IOC" / "FOK"`
# Swap 结算方式
SettleStr = str  # `"usd_margin" / "coin_margin"`


# ═══════════════════════════════════════════════════════════════════════════
# 校验工具
# ═══════════════════════════════════════════════════════════════════════════

# 合法 side 集合(小写,用于大小写不敏感比较)
_VALID_SIDES: frozenset[str] = frozenset({"buy", "sell"})
# 合法 instrument kind
_VALID_INSTRUMENT_KINDS: frozenset[str] = frozenset({"spot", "swap"})
# 合法 swap settle
_VALID_SWAP_SETTLES: frozenset[str] = frozenset({"usd_margin", "coin_margin"})


def _validate_instrument(instrument: Any) -> None:
    """校验 `instrument` dict 结构(0.5.0 新增)。

    Args:
        instrument: 由 `spot_instrument()` / `swap_instrument()` 构造或
            手写的 dict,字段格式需与 Rust `parse_instrument` 对齐。

    Raises:
        TypeError: 非 dict 类型
        KeyError: 缺 `kind` / `base` / `quote`(swap 还需 `settle` / `contract_size`)
        ValueError: `kind` / `settle` 值非法,或 `contract_size` 非正数
    """
    if not isinstance(instrument, dict):
        raise TypeError(
            f"instrument must be a dict, got {type(instrument).__name__}; "
            "use spot_instrument() / swap_instrument() to construct"
        )

    if "kind" not in instrument:
        raise KeyError("missing 'kind' in instrument dict")
    kind_raw = str(instrument["kind"]).strip().lower()
    if kind_raw not in _VALID_INSTRUMENT_KINDS:
        raise ValueError(
            f"invalid instrument kind: {instrument['kind']!r} "
            "(expected 'spot' / 'swap')"
        )

    if "base" not in instrument:
        raise KeyError("missing 'base' in instrument dict")
    if "quote" not in instrument:
        raise KeyError("missing 'quote' in instrument dict")
    if not str(instrument["base"]).strip():
        raise ValueError("instrument 'base' must be non-empty")
    if not str(instrument["quote"]).strip():
        raise ValueError("instrument 'quote' must be non-empty")

    if kind_raw == "swap":
        if "settle" not in instrument:
            raise KeyError("missing 'settle' in swap instrument dict")
        settle_raw = str(instrument["settle"]).strip().lower()
        if settle_raw not in _VALID_SWAP_SETTLES:
            raise ValueError(
                f"invalid swap settle: {instrument['settle']!r} "
                "(expected 'usd_margin' / 'coin_margin')"
            )
        if "contract_size" not in instrument:
            raise KeyError("missing 'contract_size' in swap instrument dict")
        contract_size = float(instrument["contract_size"])
        if contract_size <= 0.0:
            raise ValueError(
                f"swap contract_size must be positive, got {contract_size}"
            )


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
    "InstrumentDict",
    "FillDict",
    "SubmitResultDict",
    "BatchModeStr",
    "SideStr",
    "TifStr",
    "SettleStr",
    # 工厂函数
    "limit_order",
    "market_order",
    "spot_instrument",
    "swap_instrument",
]


# ═══════════════════════════════════════════════════════════════════════════
# Instrument dict 工厂(0.5.0 新增)
# ═══════════════════════════════════════════════════════════════════════════


def spot_instrument(base: str, quote: str) -> InstrumentDict:
    """构造 spot instrument dict(供 L1/L2/L3/BacktestEngine 接收)。

    0.5.0 起:`Order` dict 用 `instrument` 子字段代替旧的 `symbol` 字符串,
    以支持 spot / swap 区分。本工厂返回 Rust 端 `parse_instrument` 能直接
    解析的 wire 格式,无需手写字典。

    Args:
        base: 基础币种(交易标的),如 `"BTC"`
        quote: 计价币种,如 `"USDT"`

    Returns:
        dict:`{"kind": "spot", "base": "BTC", "quote": "USDT"}`

    Raises:
        ValueError: `base` / `quote` 为空字符串

    Examples::

        btc_usdt = spot_instrument("BTC", "USDT")
        engine.submit(limit_order(1, btc_usdt, "Buy", 50_000.0, 0.1))
    """
    base_str = str(base).strip()
    quote_str = str(quote).strip()
    if not base_str:
        raise ValueError("spot_instrument: 'base' must be non-empty")
    if not quote_str:
        raise ValueError("spot_instrument: 'quote' must be non-empty")
    return {"kind": "spot", "base": base_str, "quote": quote_str}


def swap_instrument(
    base: str,
    quote: str,
    settle: SettleStr = "usd_margin",
    contract_size: float = 1.0,
) -> InstrumentDict:
    """构造 swap(永续合约)instrument dict。

    Args:
        base: 基础币种(交易标的),如 `"BTC"`
        quote: 计价币种,如 `"USDT"`
        settle: 结算方式,`"usd_margin"`(默认,USD 保证金) /
            `"coin_margin"`(币本位保证金),大小写不敏感
        contract_size: 合约乘数(每张合约代表多少 base 币),默认 1.0
            即 1 张 = 1 BTC。Binance BTCUSDT 永续默认 1,
            部分小币种合约 0.001 / 0.01 / 100 等。

    Returns:
        dict:`{"kind": "swap", "base": "BTC", "quote": "USDT",
        "settle": "usd_margin", "contract_size": 1.0}`

    Raises:
        ValueError: `base` / `quote` 为空,settle 非法,contract_size 非正

    Examples::

        # USD 保证金 永续(Binance BTCUSDT perp 默认)
        btc_perp = swap_instrument("BTC", "USDT", settle="usd_margin", contract_size=1.0)
        # 币本位 永续
        btc_coin_perp = swap_instrument("BTC", "USD", settle="coin_margin", contract_size=1.0)
    """
    base_str = str(base).strip()
    quote_str = str(quote).strip()
    if not base_str:
        raise ValueError("swap_instrument: 'base' must be non-empty")
    if not quote_str:
        raise ValueError("swap_instrument: 'quote' must be non-empty")
    settle_norm = str(settle).strip().lower()
    if settle_norm not in _VALID_SWAP_SETTLES:
        raise ValueError(
            f"swap_instrument: invalid settle {settle!r} "
            "(expected 'usd_margin' / 'coin_margin')"
        )
    contract_size_f = float(contract_size)
    if contract_size_f <= 0.0:
        raise ValueError(
            f"swap_instrument: contract_size must be positive, got {contract_size_f}"
        )
    return {
        "kind": "swap",
        "base": base_str,
        "quote": quote_str,
        "settle": settle_norm,
        "contract_size": contract_size_f,
    }


# ═══════════════════════════════════════════════════════════════════════════
# Order dict 工厂
# ═══════════════════════════════════════════════════════════════════════════


def limit_order(
    order_id: int,
    instrument: InstrumentDict,
    side: SideStr,
    price: float,
    quantity: float,
    tif: TifStr = "GTC",
) -> OrderDict:
    """构造 limit order dict(供 L1/L2/L3/MultiAsset.submit / BacktestEngine.push_event 接收)。

    0.5.0 起:**`symbol: str` 被 `instrument: dict` 取代**,签名变化是
    BREAKING CHANGE。所有 spot / swap 区分通过 `instrument` 字段表达,
    不再依赖 `symbol` 字符串约定。

    必填字段:`id` / `instrument` / `side` / `type` / `price` / `quantity` / `tif`

    Args:
        order_id: 订单 ID(全局唯一,整数)
        instrument: 交易品种 dict,由 [`spot_instrument`] / [`swap_instrument`]
            工厂构造,或手写但需匹配 Rust 端 `parse_instrument` 协议
        side: 订单方向,`"Buy"` / `"Sell"`(大小写不敏感,Rust 端统一小写匹配)
        price: 限价单价
        quantity: 订单数量(浮点,内部以 `Quantity::from_f64` 转换)
        tif: 有效期,`"GTC"`(默认) / `"IOC"` / `"FOK"`

    Returns:
        dict,字段对应 Rust 端 `Order` 字段

    Raises:
        TypeError: `instrument` 非 dict
        KeyError: `instrument` 缺 `kind` / `base` / `quote`
        ValueError: `instrument` 字段值非法,或 `side` / `tif` 非法

    Examples::

        btc = spot_instrument("BTC", "USDT")
        o = limit_order(1, btc, "Buy", 50_000.0, 0.1)
        engine.submit(o)
    """
    _validate_instrument(instrument)
    side_norm = str(side).strip().lower()
    if side_norm not in _VALID_SIDES:
        raise ValueError(
            f"invalid side: {side!r} (expected 'Buy' / 'Sell')"
        )
    tif_norm = str(tif).strip().upper()
    if tif_norm not in {"GTC", "IOC", "FOK", "GFD", "FAK"}:
        raise ValueError(
            f"invalid tif: {tif!r} (expected 'GTC' / 'IOC' / 'FOK' / 'GFD' / 'FAK')"
        )
    if float(price) <= 0.0:
        raise ValueError(f"limit order price must be positive, got {price}")
    if float(quantity) <= 0.0:
        raise ValueError(f"limit order quantity must be positive, got {quantity}")
    return {
        "id": int(order_id),
        "instrument": dict(instrument),  # 防御性拷贝,避免外部 mutate
        "side": str(side),
        "type": "limit",
        "price": float(price),
        "quantity": float(quantity),
        "tif": tif_norm,
    }


def market_order(
    order_id: int,
    instrument: InstrumentDict,
    side: SideStr,
    quantity: float,
) -> OrderDict:
    """构造 market order dict(供 L1/L2/L3/MultiAsset.submit / BacktestEngine.push_event 接收)。

    市价单**不**需要 `price` 字段(以对手盘最优价即时成交),
    `tif` 强制为 `"IOC"`(立即成交否则取消),与 Rust 端行为一致。

    0.5.0 起:**`symbol: str` 被 `instrument: dict` 取代**。

    Args:
        order_id: 订单 ID
        instrument: 交易品种 dict,由 [`spot_instrument`] / [`swap_instrument`] 构造
        side: 订单方向
        quantity: 订单数量

    Returns:
        dict,字段对应 Rust 端 `Order::Market` 变体

    Raises:
        TypeError / KeyError / ValueError: 参见 [`limit_order`]
    """
    _validate_instrument(instrument)
    side_norm = str(side).strip().lower()
    if side_norm not in _VALID_SIDES:
        raise ValueError(
            f"invalid side: {side!r} (expected 'Buy' / 'Sell')"
        )
    if float(quantity) <= 0.0:
        raise ValueError(f"market order quantity must be positive, got {quantity}")
    return {
        "id": int(order_id),
        "instrument": dict(instrument),
        "side": str(side),
        "type": "market",
        "quantity": float(quantity),
        "tif": "IOC",
    }
