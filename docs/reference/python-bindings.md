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

### `axon_quant.backtest`

- `L1MatchingEngine` / `L2MatchingEngine` / `L3MatchingEngine`
- `BacktestConfig` / `BacktestResult`
- `make_env()` —— 构造回测环境

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
