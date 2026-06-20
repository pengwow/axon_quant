# Python Bindings

> Applicable version: AXON v0.1.0+ Python bindings (Stage K delivery)

AXON exposes core Rust types to Python via PyO3, providing the `axon_quant` package.

## Installation

```bash
# 1. Prepare Python 3.14.6 virtual environment (pyenv managed)
pyenv install 3.14.6
pyenv virtualenv 3.14.6 axon_quant
pyenv local axon_quant
pyenv shell axon_quant

# 2. Compile and install
make python-install

# 3. Verify
python -c "import axon_quant; print(axon_quant.__version__)"
# Expected output: 0.1.0
```

## Core Modules

### axon_quant.rl — Reinforcement Learning

```python
import axon_quant.rl as rl

# Create trading environment
env = rl.TradingEnv(
    config={"initial_capital": 100_000.0, "max_steps": 500},
    market_data=bars,
    action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    reward="sharpe",
)

# Standard Gymnasium interface
obs = env.reset()
obs, reward, terminated, truncated, info = env.step([0.5])
```

### axon_quant.llm — LLM Integration

```python
import axon_quant.llm as llm

# Create LLM backend
backend = llm.make_backend({
    "backends": [
        {"type": "openai_compat", "api_key": "your-key", "model": "gpt-4"}
    ]
})

# Create ReAct agent
agent = llm.ReActAgent(backend=backend, tools=[...])

# Run trading loop
response = agent.chat([llm.LLMMessage.user("Analyze BTC market")])
```

### axon_quant.hpo — Hyperparameter Optimization

```python
import axon_quant.hpo as hpo

# Define search space
search_space = {
    "learning_rate": hpo.SearchSpaceDef(param_type="log_uniform", low=1e-5, high=1e-3),
    "gamma": hpo.SearchSpaceDef(param_type="uniform", low=0.95, high=0.999),
}

# Create HPO runner
runner = hpo.OptunaHPO(
    search_space=search_space,
    objective_fn=objective_fn,
    study_name="ppo_optimization",
    directions=["maximize", "maximize"],
)

# Run optimization
results = runner.run(n_trials=50)
```

### axon_quant.tracker — Experiment Tracking

```python
import axon_quant.tracker as tracker

# Create MLflow tracker
t = tracker.MLflowTracker(
    tracking_uri="http://localhost:5000",
    experiment_name="ppo_btc",
)

# Log parameters and metrics
t.log_param("learning_rate", 3e-4)
t.log_metric("sharpe_ratio", 1.5, step=100)

# Finish run
t.finish(tracker.RunStatus.Success)
```

### axon_quant.registry — Model Registry

```python
import axon_quant.registry as registry

# Create registry
storage = registry.LocalStorage.new(base_dir="./models")
reg = registry.ModelRegistry.new(storage)

# Register model
version = reg.register(
    name="ppo_btc",
    artifact_path="./model.onnx",
    metadata={"algorithm": "PPO", "sharpe": "1.5"},
)

# Promote to production
reg.transition_stage("ppo_btc", version.version, registry.ModelStage.PRODUCTION)
```

### axon_quant.inference — Model Inference

```python
import axon_quant.inference as inference

# Create ONNX backend
engine = inference.OnnxBackend(
    model_path="model.onnx",
    device="cuda:0",
    input_shape=[1, 64, 128],
)

# Load model
engine.load()

# Run inference
action = engine.infer(observation)
```

### axon_quant.exchange — Exchange Integration

```python
import axon_quant.exchange as exchange

# Create Binance adapter
adapter = exchange.BinanceAdapter(
    api_key="your-key",
    api_secret="your-secret",
    testnet=True,
)

# Connect and subscribe
await adapter.connect()
await adapter.subscribe(["BTCUSDT"])

# Place order
order_id = await adapter.send_order(
    symbol="BTCUSDT",
    side="BUY",
    quantity=0.001,
    order_type="MARKET",
)
```

### axon_quant.backtest (Stage 2) — Event-Driven Backtest

Backtest engine with L1/L2/L3 matching engines, market impact modeling, and event-driven replay loop.

| Class | Description |
|-------|-------------|
| `L1MatchingEngine` | Price-time priority matching (basic) |
| `L2MatchingEngine` | Advanced: `modify` / `from_entries` / `export_entries` / `volume_at_price` / `stats` / `location` |
| `MultiAssetMatchingEngine` | Multi-asset routing + dark pool + batch auction + arbitrage detection |
| `ImpactedMatchingEngine` | Impact-aware matching (linear / power_law models + Python custom models) |
| `ImpactedMatchingEngineBuilder` | Builder-style construction of impact-aware engine |
| `BacktestEngine` | Event-driven backtest main loop (`order_submitted` / `order_cancelled` / `order_modified` / `fill`) |
| `RunResult` / `RunStats` | Backtest results (events_processed / fills / PnL / drawdown / final_nav) |
| `BacktestError` | Matching exception (inherits `Exception`, **not** `AxonError`, to avoid cargo cycle) |
| `OrderBookEntry` | L2 order book entry (for `from_entries` import) |
| `DarkOrder` / `CrossPair` / `AuctionResult` / `ArbitrageOpportunity` | L3 dark pool / cross-asset / auction / arbitrage data types |
| `limit_order(id, symbol, side, price, quantity, tif="GTC")` | Factory returning a limit order dict |
| `market_order(id, symbol, side, quantity)` | Factory returning a market order dict (tif forced to IOC) |

#### Example: Basic matching + impact

```python
from axon_quant.backtest import (
    L1MatchingEngine, ImpactedMatchingEngineBuilder,
    BacktestEngine, limit_order,
)

# 1) Basic matching
engine = L1MatchingEngine()
engine.submit(limit_order(1, "BTC-USDT", "Sell", 100.0, 1.0))
result = engine.submit(limit_order(2, "BTC-USDT", "Buy", 100.0, 1.0))
print(result["is_filled"], len(result["fills"]))  # True, 1

# 2) Impact-aware (builder chain)
ie = (ImpactedMatchingEngineBuilder()
      .model_type("linear")
      .coefficient(0.1)
      .depth_levels(5)
      .build())
ie.submit(limit_order(3, "BTC-USDT", "Buy", 100.0, 1.0))
print(ie.permanent_offset())  # Cumulative permanent impact offset

# 3) Event-driven backtest
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

#### Submit Order Return Protocol

All `submit` calls uniformly return:

```python
{
    "is_filled": bool,              # Fully filled
    "is_partially_filled": bool,    # Partially filled
    "remaining_quantity": float,    # Unfilled quantity
    "fills": [                      # List of fills
        {
            "fill_id": int,
            "taker_order_id": int,
            "maker_order_id": int,
            "price": float,
            "quantity": float,
            "taker_side": "BUY" | "SELL",  # Uppercase
        },
        ...
    ],
}
```

#### BacktestEngine Event Types

| `type` field | Required fields | Meaning |
|--------------|-----------------|---------|
| `order_submitted` | `order: dict` | Submit an order |
| `order_cancelled` | `order_id: int` | Cancel an order |
| `order_modified` | `order_id: int` + `new_price` / `new_quantity` | Modify an order |
| `fill` | `price` / `quantity` / `buyer_order_id` / `seller_order_id` | External fill (bypass matching) |

#### `BacktestError` Exception System

`BacktestError` inherits directly from builtin `Exception` (a `PyException` subclass) and **does not** inherit from the Stage 1 `AxonError` base class. Design rationale: having `axon-backtest` depend on `axon-python::AxonError` would create a cargo cycle (since `axon-python` depends on `axon-backtest`), so the Rust side keeps no hard dependency. The Python thin wrapper injects a pseudo-inheritance via `__bases__` as a fallback:

```python
try:
    axon_quant.backtest.L1MatchingEngine().submit(bad_order)
except axon_quant.backtest.BacktestError as e:  # Actually an Exception subclass
    code = e.args[0]    # e.g. "Matching"
    msg = e.args[1]     # e.g. "[Matching] invalid side: xxx"
```

| Error Code | Meaning |
|------------|---------|
| `Matching` | L1/L2 matching error (order not found / invalid price / invalid quantity) |
| `MatchingL3` | L3 multi-asset matching error (asset not registered / invalid cross-asset params) |

## Type Mapping

| Python Type | Rust Type | Description |
|-------------|-----------|-------------|
| `float` | `f64` | 64-bit float |
| `int` | `i64` | 64-bit integer |
| `str` | `String` | UTF-8 string |
| `list` | `Vec<T>` | Dynamic array |
| `dict` | `HashMap<K, V>` | Hash map |
| `tuple` | `(T1, T2, ...)` | Tuple |
| `None` | `Option<T>::None` | Optional value |
| `bytes` | `Vec<u8>` | Byte array |

## Error Handling

Python bindings raise `axon_quant.AxonError` for Rust errors:

```python
import axon_quant

try:
    result = some_operation()
except axon_quant.AxonError as e:
    print(f"Error: {e}")
    print(f"Error type: {type(e).__name__}")
```

## Async Support

Many Python bindings are synchronous wrappers around async Rust functions. For high-throughput scenarios, consider using the async API directly:

```python
import asyncio
import axon_quant.exchange as exchange

async def main():
    adapter = exchange.BinanceAdapter(...)
    await adapter.connect()
    
    # Async operations
    balances = await adapter.get_balances()
    order_id = await adapter.place_order(...)

asyncio.run(main())
```

## Next Steps

- [API Reference](api-reference.md) — Complete API documentation
- [Configuration](configuration.md) — Configuration options
- [Quick Start](../getting-started/quickstart.md) — Get started with Python
