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
