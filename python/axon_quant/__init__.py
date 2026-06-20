"""AXON Quant - 量化交易回测与强化学习框架

Rust 核心 + Python RL 接口，从回测到生产的全链路统一框架。

子模块：
- ``data`` — 数据服务（多源接入、L1/L2 缓存、PyArrow zero-copy，Stage 1）
- ``backtest`` — 回测引擎（L1/L2/L3 撮合 + impact + BacktestEngine，Stage 2）
- ``rl`` — Gymnasium 兼容的 RL 交易环境（TradingEnv / VecEnv）
- ``hpo`` — 超参数优化（Optuna 集成 / 多目标 / 剪枝）
- ``walk_forward`` — 滚动前向验证（purge / embargo / 泄漏检测）
- ``tracker`` — 实验追踪（Memory / Local / MLflow / WandB）
- ``registry`` — 模型注册表（版本管理 / 生命周期 / 本地存储）
- ``distributed`` — 分布式训练（Ray / 参数服务器 / 检查点）
- ``llm`` — LLM 后端（OpenAI 兼容协议，多厂商主备，PyO3 绑定）
- ``trading`` — Trading 工具（PyO3 绑定:MockTradingBackend / 下单/撤单/改单/查询/指标）

用法::

    import axon_quant

    env = axon_quant.rl.TradingEnv(
        config={"initial_capital": 100_000.0, "max_steps": 1000},
        action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    )

    # 数据服务（Stage 1）:L1/L2 缓存 + PyArrow zero-copy
    import datetime
    from axon_quant.data import DataService, MockSource, Frequency, DataRequest
    svc = (
        DataService.new()
        .register_source(MockSource.with_tick_series("btc", 1000, 1_000_000, lambda i: 100.0 + i))
        .with_cache_capacity(64)
    )
    req = DataRequest(
        "BTCUSDT",
        datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc),
        datetime.datetime(2026, 1, 2, tzinfo=datetime.timezone.utc),
        Frequency.Tick,
    )
    ds = svc.load(req)
    print(ds.len, ds.checksum[:8])
    batch = ds.to_arrow(0)  # zero-copy pyarrow.RecordBatch

    # LLM 后端:主动传参,避免依赖环境变量
    from axon_quant.llm import LLMConfig, make_backend, LLMMessage
    backend = make_backend(LLMConfig(backends=[{
        "base_url": "https://api.example.com/v1",
        "api_key": "sk-xxx",
        "model": "model-name",
    }]))
    print(backend.chat([LLMMessage("user", "Hi!")])["content"])

    # Trading 工具(Stage K):mock 闭环
    from axon_quant.trading import (
        RiskLimits, MockTradingBackend,
        PlaceOrderTool, QueryPortfolioTool,
    )
    backend = MockTradingBackend()
    risk = RiskLimits(allowed_symbols=["BTC-USDT"])
    place = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk)
    ack = place.execute({
        "symbol": "BTC-USDT",
        "side": "Buy",
        "quantity": 0.1,
        "price": 50000.0,
    })
    print(ack["status"])  # "DryRun"

    # 回测引擎(Stage 2):L1 撮合 + 事件驱动回测
    from axon_quant.backtest import (
        L1MatchingEngine, BacktestEngine, limit_order, BacktestError,
    )
    engine = L1MatchingEngine()
    engine.submit(limit_order(1, "BTCUSDT", "Sell", 100.0, 1.0))
    result = engine.submit(limit_order(2, "BTCUSDT", "Buy", 100.0, 1.0))
    print(result["is_filled"], len(result["fills"]))  # True 1

    bt = BacktestEngine(initial_cash=100_000.0)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, "BTCUSDT", "Buy", 100.0, 1.0),
    })
    print(bt.run().final_nav)

    # 风控引擎(Stage 3):预交易检查 + 熔断器 + 风险指标
    from axon_quant.risk import (
        DefaultRiskEngine, RiskConfig, CircuitBreaker,
        make_order, make_portfolio, make_risk_config,
    )
    rengine = DefaultRiskEngine(make_risk_config(max_order_value=1000.0))
    rorder = make_order(id=1, symbol="BTC-USDT", side="Buy",
                        type="limit", price=100.0, quantity=1.0)
    rportfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    rresult = rengine.check_order(rorder, rportfolio)
    print(rresult.is_allow)  # True
"""

from __future__ import annotations

# 从原生 Rust 扩展导入所有符号
from ._native import *  # noqa: F401, F403

# 重新导出原生子模块（由 Rust PyO3 注册）
# 注意:`llm` 和 `trading` 是从纯 Python `axon_quant.{llm,trading}` 模块导出(见下方),
# 因为我们对它们做了 Python 端的封装(类型别名 + dataclass),
# 不直接 re-export 原生的 `_native.llm` / `_native.trading`。
from ._native import (  # noqa: F401
    __version__,  # noqa: F401
    backtest,      # Stage 2:axon-backtest 暴露
    data,           # Stage 1:axon-data 暴露
    distributed,
    hpo,
    registry,
    rl,
    tracker,
    walk_forward,
)

# 重新导出 backtest 顶层 Python API(包装 _native.backtest,Stage 2)
from .backtest import (  # noqa: F401
    ArbitrageOpportunity,
    AuctionResult,
    BacktestEngine,
    BacktestError,
    CrossPair,
    DarkOrder,
    ImpactedMatchingEngine,
    ImpactedMatchingEngineBuilder,
    L1MatchingEngine,
    L2MatchingEngine,
    MultiAssetMatchingEngine,
    OrderBookEntry,
    RunResult,
    RunStats,
    limit_order,
    market_order,
)

# 重新导出 data 顶层 Python API(包装 _native.data,Stage 1)
from .data import (  # noqa: F401
    AxonError,
    CacheControl,
    CacheStats,
    DataError,
    DataRequest,
    DataService,
    Dataset,
    DataType,
    Frequency,
    MockSource,
    SchemaField,
    Tick,
)

# 重新导出 LLM 顶层 Python API(包装 _native.llm)
# 这里必须用 `from .llm import ...` 而非 `from . import llm`,
# 后者会优先复用 sys.modules['axon_quant.llm'] 缓存,
# 而该缓存可能已经被 `from ._native import *` 注入为原生 _native.llm 引用。
from .llm import (  # noqa: F401
    LLMBackend,
    LLMConfig,
    LLMMessage,
    load_config_from_toml,
    make_backend,
)

# 重新导出 trading 顶层 Python API(包装 _native.trading,Stage K)
from .trading import (  # noqa: F401
    CancelOrderTool,
    MockTradingBackend,
    PlaceOrderTool,
    QueryPortfolioTool,
    ReplaceOrderTool,
    RiskLimits,
    TradingMetrics,
)

# 让 `axon_quant.llm` / `axon_quant.trading` 这些子模块也对外可见(给文档 / 静态分析使用)
# noqa: F405 是因为 ruff 误判 llm / trading 来自 `from ._native import *`,
# 实际上下方 `from .llm import ...` / `from .trading import ...` 并没有显式 import
# `llm` / `trading` 这两个模块对象
__all__ = [  # noqa: F405
    "__version__",
    "data",        # Stage 1
    "backtest",    # Stage 2
    "risk",        # Stage 3
    "rl",
    "hpo",
    "walk_forward",
    "tracker",
    "registry",
    "distributed",
    "llm",
    "trading",
    "AxonError",   # Stage 1:异常基类
    "DataError",   # Stage 1:数据服务异常
    # Stage 2:backtest 顶层 API
    "L1MatchingEngine",
    "L2MatchingEngine",
    "MultiAssetMatchingEngine",
    "ImpactedMatchingEngine",
    "ImpactedMatchingEngineBuilder",
    "OrderBookEntry",
    "DarkOrder",
    "CrossPair",
    "AuctionResult",
    "ArbitrageOpportunity",
    "BacktestEngine",
    "RunResult",
    "RunStats",
    "BacktestError",
    "limit_order",
    "market_order",
    "DataService", # Stage 1
    "DataRequest", # Stage 1
    "DataType",    # Stage 1
    "Frequency",   # Stage 1
    "MockSource",  # Stage 1
    "Tick",        # Stage 1
    "Dataset",     # Stage 1
    "SchemaField", # Stage 1
    "CacheStats",  # Stage 1
    "CacheControl",# Stage 1
    # Stage 3:risk 顶层 API
    "DefaultRiskEngine",
    "RiskConfig",
    "CircuitBreaker",
    "RiskMetrics",
    "RiskResult",
    "RiskReason",
    "RiskError",
    "make_order",
    "make_portfolio",
    "make_portfolio_with_positions",
    "make_risk_config",
    "make_circuit_breaker",
    "LLMConfig",
    "LLMBackend",
    "LLMMessage",
    "make_backend",
    "load_config_from_toml",
    "RiskLimits",
    "MockTradingBackend",
    "PlaceOrderTool",
    "QueryPortfolioTool",
    "CancelOrderTool",
    "ReplaceOrderTool",
    "TradingMetrics",
]
