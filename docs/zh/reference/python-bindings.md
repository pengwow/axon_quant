# Python 绑定

> 适用版本:AXON v0.1.0+ Python 绑定(Stage K 交付)

AXON 通过 PyO3 把核心 Rust 类型暴露给 Python,提供 `axon_quant` 包。

## 安装

```bash
# 1. 准备 Python 3.14.6 虚拟环境(pyenv 管理)
pyenv install 3.14.6
pyenv virtualenv 3.14.6 axon_quant
pyenv local axon_quant
pyenv shell axon_quant

# 2. 编译并安装
make python-install

# 3. 验证
python -c "import axon_quant; print(axon_quant.__version__)"
```

## 核心模块

### `axon_quant.core`

- `Order` / `OrderSide` / `OrderType` / `TimeInForce`
- `MarketData` / `Tick` / `Bar`
- `Portfolio` / `Position` / `Cash`

### `axon_quant.backtest`(Stage 2 交付)

事件驱动回测引擎,含 L1/L2/L3 撮合引擎 + 市场冲击感知 + 事件回放主循环。

| 类 | 说明 |
|----|------|
| `L1MatchingEngine` | 价格-时间优先撮合(基础) |
| `L2MatchingEngine` | 进阶:`modify` / `from_entries` / `export_entries` / `volume_at_price` / `stats` / `location` |
| `MultiAssetMatchingEngine` | 多资产路由 + 暗池 + 批量拍卖 + 套利检测 |
| `ImpactedMatchingEngine` | 冲击感知撮合(支持 linear / power_law 模型 + Python 自定义模型) |
| `ImpactedMatchingEngineBuilder` | 链式构造冲击感知引擎 |
| `BacktestEngine` | 事件驱动回测主循环(`order_submitted` / `order_cancelled` / `order_modified` / `fill` 4 种事件) |
| `RunResult` / `RunStats` | 回测结果(events_processed / fills / PnL / drawdown / final_nav) |
| `BacktestError` | 撮合异常(继承 `Exception`,**不**继承 `AxonError`,避免 cargo 循环) |
| `OrderBookEntry` | L2 订单簿条目(用于 `from_entries` 导入) |
| `DarkOrder` / `CrossPair` / `AuctionResult` / `ArbitrageOpportunity` | L3 暗池 / 跨资产 / 拍卖 / 套利数据结构 |
| `limit_order(id, symbol, side, price, quantity, tif="GTC")` | 工厂函数,返回限价单 dict |
| `market_order(id, symbol, side, quantity)` | 工厂函数,返回市价单 dict(tif 强制 IOC) |

#### 示例:基础撮合 + 冲击感知

```python
from axon_quant.backtest import (
    L1MatchingEngine, ImpactedMatchingEngineBuilder,
    BacktestEngine, limit_order,
)

# 1) 基础撮合
engine = L1MatchingEngine()
engine.submit(limit_order(1, "BTC-USDT", "Sell", 100.0, 1.0))
result = engine.submit(limit_order(2, "BTC-USDT", "Buy", 100.0, 1.0))
print(result["is_filled"], len(result["fills"]))  # True, 1

# 2) 冲击感知(Builder 链式)
ie = (ImpactedMatchingEngineBuilder()
      .model_type("linear")
      .coefficient(0.1)
      .depth_levels(5)
      .build())
ie.submit(limit_order(3, "BTC-USDT", "Buy", 100.0, 1.0))
print(ie.permanent_offset())  # 累计永久冲击偏移

# 3) 事件驱动回测
bt = BacktestEngine(initial_cash=100_000.0)
bt.push_event({
    "type": "order_submitted",
    "timestamp_ns": 1_000,
    "order": limit_order(1, "BTC-USDT", "Sell", 100.0, 1.0),
})
bt.push_event({
    "type": "order_submitted",
    "timestamp_ns": 2_000,
    "order": limit_order(2, "BTC-USDT", "Buy", 100.0, 1.0),
})
result = bt.run()
print(result.events_processed, result.fills, result.final_nav)
```

#### 提交订单返回 dict 协议

所有 `submit` 调用统一返回:

```python
{
    "is_filled": bool,              # 是否全部成交
    "is_partially_filled": bool,    # 是否部分成交
    "remaining_quantity": float,    # 剩余未成交量
    "fills": [                      # 成交列表
        {
            "fill_id": int,
            "taker_order_id": int,
            "maker_order_id": int,
            "price": float,
            "quantity": float,
            "taker_side": "BUY" | "SELL",  # 全大写
        },
        ...
    ],
}
```

#### BacktestEngine 事件类型

| `type` 字段 | 必填字段 | 含义 |
|-------------|----------|------|
| `order_submitted` | `order: dict` | 提交订单 |
| `order_cancelled` | `order_id: int` | 撤销订单 |
| `order_modified` | `order_id: int` + `new_price` / `new_quantity` | 修改订单 |
| `fill` | `price` / `quantity` / `buyer_order_id` / `seller_order_id` | 外部成交(旁路撮合) |

#### `BacktestError` 异常体系

`BacktestError` **直接**继承 builtin `Exception`(`PyException` 子类),**不**继承 Stage 1 `AxonError` 基类。设计原因:`axon-backtest` 反向依赖 `axon-python::AxonError` 会造成 cargo 循环(`axon-python` 依赖 `axon-backtest`),所以 Rust 侧不硬依赖。Python 端 thin wrapper 走 `__bases__` 伪继承兜底:

```python
try:
    axon_quant.backtest.L1MatchingEngine().submit(bad_order)
except axon_quant.backtest.BacktestError as e:  # 实际是 Exception 子类
    code = e.args[0]    # e.g. "Matching"
    msg = e.args[1]     # e.g. "[Matching] invalid side: xxx"
```

| 错误码 | 含义 |
|--------|------|
| `Matching` | L1/L2 撮合错误(订单未找到 / 非法价格 / 非法数量) |
| `MatchingL3` | L3 多资产撮合错误(资产未注册 / 跨资产参数非法) |

### `axon_quant.risk`(Stage 3 交付)

预交易风控引擎,含 8 项风控阈值 + 独立熔断器 + 风险指标聚合 + 组合监控告警。

| 类 | 说明 |
|----|------|
| `DefaultRiskEngine` | 风控主类:`check_order` / `check_portfolio` / `update_daily_pnl` / `reset_daily` / `metrics` |
| `RiskConfig` | 8 项风控阈值配置(单标持仓 / 总敞口 / 单笔价值 / 杠杆 / 回撤 / 日内亏损 / 集中度 / 熔断冷却) |
| `CircuitBreaker` | 独立熔断器:`check_and_trigger` / `reset` / `is_active`(不依赖 `DefaultRiskEngine`) |
| `RiskMetrics` | 风险指标聚合(NAV / 杠杆 / 回撤 / 日内 PnL / VaR(95) / 集中度) |
| `RiskResult` | 检查结果(Allow / Reject(reason) / Warn(msg)),用 `kind` 标签模式(非 PyO3 enum) |
| `RiskReason` | 拒绝原因(8 个变体扁平化):`OrderTooLarge` / `PositionLimitExceeded` / `MaxLeverageExceeded` / `MaxDrawdownExceeded` / `DailyPnLLimit` / `CircuitBreakerActive` / `ConcentrationTooHigh` / `InsufficientMargin` |
| `RiskError` | 风控异常(继承 `Exception`,**不**继承 `AxonError`,避免 cargo 循环) |
| `make_order(...)` | 工厂函数,返回订单 dict(限价/市价) |
| `make_portfolio(...)` | 工厂函数,返回最简 portfolio dict(只填 base_currency / commission_rate) |
| `make_portfolio_with_positions(...)` | 工厂函数,返回含 cash + positions 的 portfolio dict |
| `make_risk_config(...)` | 工厂函数,返回 `RiskConfig` 实例 |
| `make_circuit_breaker(...)` | 工厂函数,返回 `CircuitBreaker` 实例 |

#### 示例:预交易风控 + 熔断 + 风险指标

```python
from axon_quant.risk import (
    DefaultRiskEngine, RiskConfig, CircuitBreaker,
    RiskResult, RiskReason, RiskMetrics, RiskError,
    make_order, make_portfolio, make_portfolio_with_positions,
    make_risk_config, make_circuit_breaker,
)

# 1) 构造风控引擎
engine = DefaultRiskEngine(make_risk_config(
    max_order_value=10_000.0,     # 单笔订单最大价值
    max_leverage=2.0,              # 最大杠杆倍数
    max_daily_loss=5_000.0,        # 日内最大亏损(触发熔断)
    max_concentration=0.30,        # 单一标的占组合最大比例
))

# 2) 构造订单 + 组合
order = make_order(
    id=1, symbol="BTC-USDT", side="Buy",
    type="limit", price=100.0, quantity=1.0,
)
portfolio = make_portfolio(
    base_currency="USD",
    commission_rate=0.001,
    cash={"USD": 100_000.0},
)

# 3) 预交易检查
result = engine.check_order(order, portfolio)
if result.is_allow:
    print("Order allowed")
elif result.is_reject:
    reason = result.reason
    print(f"Rejected: {reason.kind}")  # e.g. "OrderTooLarge"
else:
    print(f"Warning: {result.message}")
```

#### 累计日内 PnL 触发熔断

```python
# 触发日内亏损超阈值 → engine.check_order() 拒绝
engine.update_daily_pnl(2_000.0)    # 累计盈利
engine.update_daily_pnl(-7_500.0)   # 累计亏损超 5_000 → 熔断
r = engine.check_order(order, portfolio)
assert r.is_reject and r.reason.kind == "CircuitBreakerActive"

# 重置日内状态(不重置 VaR 历史窗口)
engine.reset_daily()
```

#### RiskReason 字段访问

```python
reason = RiskReason.from_dict({
    "kind": "OrderTooLarge",
    "max": 10_000.0,
    "actual": 20_000.0,
})
assert reason.kind == "OrderTooLarge"
assert reason.get("max") == 10_000.0
assert reason.get("actual") == 20_000.0
d = reason.to_dict()  # {"kind": "OrderTooLarge", "max": 10000.0, "actual": 20000.0}
```

#### RiskMetrics 独立类

```python
# 引擎内部产出
m_dict = engine.metrics(portfolio)
# {"total_exposure": 100000.0, "leverage": 1.5, "current_drawdown": 0.05,
#  "daily_realized_pnl": 500.0, "var_95": 1500.0,
#  "concentration": {"BTC-USDT": 0.45, "ETH-USDT": 0.20}}

# 独立构造(测试 / 序列化的反序列化场景)
m = RiskMetrics.from_dict(m_dict)
assert m.total_exposure == 100_000.0
assert m.leverage == 1.5
assert m.concentration["BTC-USDT"] == 0.45
```

#### 独立 CircuitBreaker(不依赖 engine)

```python
cb = make_circuit_breaker(daily_loss_limit=10_000.0, cooldown_seconds=3600)
cb.check_and_trigger(-5_000.0)   # 不到阈值,未激活
assert cb.is_active is False
cb.check_and_trigger(-15_000.0)  # 触发
assert cb.is_active is True
cb.reset()                       # 强制重置
assert cb.is_active is False
```

#### Portfolio dict 协议

```python
# 最简(只填必填字段)
make_portfolio(base_currency="USD", commission_rate=0.001)

# 含 cash
make_portfolio(
    base_currency="USD",
    commission_rate=0.001,
    cash={"USD": 100_000.0, "BTC": 1.5},
)

# 含 cash + positions
make_portfolio_with_positions(
    base_currency="USD",
    cash={"USD": 50_000.0},
    positions={
        "BTC-USDT": {
            "quantity": 1.0,
            "avg_cost": 50_000.0,
            "market_price": 55_000.0,  # 可选
        },
    },
    commission_rate=0.0,
)
```

#### `RiskError` 异常体系

`RiskError` **直接**继承 builtin `Exception`(`PyException` 子类),**不**继承 Stage 1 `AxonError` 基类。设计原因:`axon-risk` 反向依赖 `axon-python::AxonError` 会造成 cargo 循环(`axon-python` 依赖 `axon-risk`),所以 Rust 侧不硬依赖。

```python
try:
    engine.check_order(bad_order, portfolio)
except axon_quant.risk.RiskError as e:  # 实际是 Exception 子类
    code = e.args[0]   # e.g. "OrderRejected" / "CircuitBreakerActive"
    msg = e.args[1]    # e.g. "[OrderRejected] Order too large: ..."
```

| 错误码 | 含义 |
|--------|------|
| `CircuitBreakerActive` | 熔断器激活,订单被拒 |
| `OrderRejected` | 订单被风控拒绝 |
| `ConfigInvalid` | 风控配置非法 |
| `Overflow` | 数值溢出 |

### `axon_quant.rl`

- `AxonEnv` —— Gymnasium 兼容环境
- `VecEnv` —— 向量化环境

### `axon_quant.llm`

- `ReActAgent` / `Tool` 基类
- `LLMBackend` / `OpenAICompatBackend` / `MockLLMBackend`
- `LLMMessage` / `LLMToolCall` / `LLMToolResult`

### `axon_quant.llm.trading`(Stage K 交付)

交易相关类型:

| 类 | 说明 |
|----|------|
| `RiskLimits` | 静态风控规则(`max_order_notional` / `max_daily_orders` / `max_position_abs` / `allowed_symbols`) |
| `SafetyMode` | `DryRun` / `TwoPhase` / `Direct` 枚举 |
| `MockTradingBackend` | 无 feature flag,默认可用,用于测试 |
| `ExchangeTradingBackend` | feature = `trading-exchange`,对接 Binance/OKX |
| `OmsTradingBackend` | feature = `trading-oms`,对接 axon-oms |
| `BacktestTradingBackend` | feature = `trading-backtest`,对接 axon-backtest |
| `PlaceOrderTool` | 下单 tool |
| `QueryPortfolioTool` | 查询持仓 / 余额 tool |
| `CancelOrderTool` | 撤单 tool |
| `ReplaceOrderTool` | 改单 tool |
| `TradingMetrics` | 指标收集器(支持 callback + snapshot) |

## 示例:LLM 交易 Mock 端到端

```python
import axon_quant
from axon_quant.llm.trading import (
    RiskLimits, SafetyMode, MockTradingBackend,
    PlaceOrderTool, QueryPortfolioTool,
)

# 1. 配置风控
risk = RiskLimits(
    max_order_notional=50_000.0,
    max_daily_orders=100,
    max_position_abs=10.0,
    allowed_symbols={"BTC-USDT", "ETH-USDT"},
)

# 2. 选后端(Mock,无 feature flag)
backend = MockTradingBackend(initial_cash=100_000.0)

# 3. 构造 tool
place_order = PlaceOrderTool(
    backend=backend,
    risk=risk,
    safety_mode=SafetyMode.DryRun,  # DryRun,不真下单
)

# 4. 调 tool
result = place_order.execute({
    "symbol": "BTC-USDT",
    "side": "Buy",
    "quantity": 0.1,
    "price": 50_000.0,
})
print(result)  # JSON 字符串
```

## 示例:Backtest 后端回放

```python
import axon_quant
from axon_quant.llm.trading import BacktestTradingBackend, PlaceOrderTool, RiskLimits

# 1. 准备历史数据
market_data = axon_quant.MarketData.from_parquet("./data/btc_2024.parquet")

# 2. Backtest 后端(对接 axon-backtest L1)
backend = BacktestTradingBackend(
    market_data=market_data,
    engine="L1",
    impact_model="almgren_chriss",
    fee_model="taker_5bps",
)

# 3. 配置 + 下单
risk = RiskLimits(max_order_notional=100_000.0, max_position_abs=1.0)
place = PlaceOrderTool(backend=backend, risk=risk, safety_mode=SafetyMode.Direct)

# 跑 LLM 决策(伪代码)
for obs in market_data:
    decision = llm_agent.decide(obs)
    if decision == "buy":
        place.execute({"symbol": "BTC-USDT", "side": "Buy", "quantity": 0.01})

# 4. 查最终 PnL
print(backend.get_balances())
print(backend.get_positions())
```

## 示例:metrics 接入 Prometheus

```python
import axon_quant
from prometheus_client import Counter, Histogram, start_http_server

orders_total = Counter(
    'trading_orders_total', 'Total trading orders',
    ['tool', 'side', 'status']
)
duration = Histogram(
    'trading_tool_execute_duration_seconds', 'Tool execution duration',
    ['tool']
)

start_http_server(9100)

def on_sample(sample):
    kind, data = sample
    if kind == 'counter_inc' and data['name'] == 'trading_orders_total':
        orders_total.labels(**data['labels']).inc(data['value'])
    elif kind == 'histogram_observe' and data['name'] == 'trading_tool_execute_duration_seconds':
        duration.labels(**data['labels']).observe(data['value_secs'])

axon_quant.set_metrics_callback(on_sample)
```

详见 [指标与告警](../user-guide/llm-trading/metrics-alerting.md) §3。

## 异步转同步

axon_quant Python 绑定内部用 `tokio::Runtime::block_on` 把 Rust async 转为 Python 同步。Python 端不需要 `asyncio`,但需要理解:

- 一次 Python 调用 = 一次完整的 Rust async 任务
- 长耗时操作(如 Backtest 跑全历史数据)可能阻塞 Python 主线程
- 建议在 `threading.Thread` 或 `concurrent.futures` 中跑

```python
from concurrent.futures import ThreadPoolExecutor

def run_backtest():
    backend = BacktestTradingBackend(...)
    # 跑完所有 LLM 决策
    return backend.get_balances()

with ThreadPoolExecutor() as ex:
    future = ex.submit(run_backtest)
    result = future.result()  # 阻塞等完成
```

## 类型映射

| Rust | Python |
|------|--------|
| `OrderSide::Buy` | `OrderSide.Buy`(枚举类) |
| `f64` | `float` |
| `i64` | `int` |
| `String` | `str` |
| `Vec<T>` | `list[T]` |
| `HashMap<String, T>` | `dict[str, T]` |
| `Result<T, E>` | 抛出异常(自定义异常类) |
| `chrono::DateTime<Utc>` | `datetime.datetime`(带 tzinfo=UTC) |

## 异常处理

| Rust 错误 | Python 异常 |
|-----------|-------------|
| `TradingError::RiskLimitsViolation` | `ValueError` |
| `TradingError::RiskGateBlocked` | `RuntimeError` |
| `TradingError::BackendError::Network` | `ConnectionError` |
| `TradingError::BackendError::Rejected` | `RuntimeError` |
| `TradingError::BackendError::InsufficientFunds` | `ValueError` |
| `TradingError::ParseError` | `ValueError` |
