# Python Bindings

> **Full runnable example**: [`examples/17_python_bindings/python_bindings_demo.py`](../../../examples/17_python_bindings/python_bindings_demo.py)
> Covers all 6 modules (Backtest / Risk / OMS / Exchange / Inference / LLM Trading). Run with one command.

> Applicable version: AXON v0.2.0+ Python bindings (Stage K delivery)

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
# Expected output: 0.2.0
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

### axon_quant.inference (Stage 6 delivery) — ONNX / Candle Inference Engine

Cross-backend inference engine with PyO3 bindings for `axon-inference` (ONNX / Candle / Tch backends + batch inference pipeline + model hot-reload). Stage 6 exposes `Onnx` and `Candle` to Python; `Tch` is intentionally not exposed (avoids PyTorch C++ linking).

| Class / Function | Purpose |
| --- | --- |
| `ModelConfig` | Model configuration: `path` / `backend` / `device` / `input_shape` (3-tuple) / `output_dim` / `fp16` / `num_threads` |
| `InferenceBackend` | Enum: `Onnx` / `Tch` / `Candle` |
| `Device` | Device: `Device.cpu()` / `Device.cuda(device_id)` / `Device.metal()` |
| `Observation` | Input observation: `symbol` / `timestamp_ns` / `features` (list[float]) |
| `ActionType` | Output enum: `Buy` / `Sell` / `Hold` / `ReduceLong` / `ReduceShort` |
| `Action` | Output: `action_type` (enum) / `confidence` (f32) / `target_position` (f32) / `model_id` / `inference_time_us` |
| `BatchConfig` | Batch pipeline: `max_batch_size` / `collect_timeout_us` / `num_workers` / `prealloc_buffer_size` / `collect_cpu_cores` / `collect_gpu_device_id` |
| `InferenceStats` | Stats: `total_inferences` / `total_batch_inferences` / `avg_latency_us` / `p99_latency_us` / `hot_reloads` / `errors` |
| `InferenceEngine` | Unified entry; `engine.load(path)` / `engine.infer(obs)` / `engine.infer_batch([obs])` / `engine.to_dict()` |
| `BatchInferencePipeline` | Simplified batch pipeline: `submit(obs)` / `collect()` / `pending()` / `stats()` |
| `ModelHotReloader` | Stage 6 stub: `__new__` returns `RuntimeError` (waiting for `engine._config()` accessor) |
| `create_onnx_engine(model_path, ...)` | One-step ONNX factory (default backend, no extra feature needed) |
| `create_candle_engine(model_path, ...)` | One-step Candle factory (requires `candle-backend` feature) |
| `create_inference_engine(config, path=None)` | Lower-level factory (also exposed as `axon_quant.create_inference_engine`) |

#### Example: ONNX single-shot inference

```python
from axon_quant.inference import (
    InferenceEngine, ModelConfig, Device, Observation, InferenceBackend,
    create_onnx_engine,
)

# One-step: create + load
engine = create_onnx_engine(
    model_path="model.onnx",
    input_shape=(1, 64, 128),
    output_dim=3,
)

# Single inference
obs = Observation(symbol="BTC-USDT", timestamp_ns=1_000_000_000, features=[0.0] * 128)
action = engine.infer(obs)
print(action.action_type, action.confidence, action.target_position)
```

#### Example: ONNX batch inference

```python
from axon_quant.inference import create_onnx_engine, Observation

engine = create_onnx_engine(model_path="model.onnx", input_shape=(1, 64, 128), output_dim=3)
obs_list = [Observation(symbol="BTC-USDT", timestamp_ns=i * 1_000, features=[0.0] * 128) for i in range(32)]
actions = engine.infer_batch(obs_list)
assert len(actions) == 32
```

#### Example: BatchInferencePipeline (buffered batch inference)

```python
from axon_quant.inference import (
    BatchInferencePipeline, BatchConfig, create_onnx_engine, Observation,
)

engine = create_onnx_engine(model_path="model.onnx", input_shape=(1, 64, 128), output_dim=3)
bcfg = BatchConfig(max_batch_size=32, collect_timeout_us=500, num_workers=2)
pipe = BatchInferencePipeline(bcfg, engine)

# Buffer observations, then trigger one batch inference
for i in range(32):
    pipe.submit(Observation(symbol="BTC-USDT", timestamp_ns=i * 1_000, features=[0.0] * 128))
print(pipe.pending())  # 32
actions = pipe.collect()
print(len(actions), pipe.stats().total_inferences)  # 32 32
```

#### Backend selection (`Onnx` / `Candle`)

```python
from axon_quant.inference import InferenceEngine, ModelConfig, Device, InferenceBackend

# Default Onnx (Stage 6 default feature, no extra compile flags needed)
engine_onnx = InferenceEngine(ModelConfig(
    path="model.onnx", backend=InferenceBackend.Onnx, device=Device.cpu(),
    input_shape=(1, 64, 128), output_dim=3,
))

# Candle (pure Rust, no ONNX runtime needed) — requires compile-time
# `candle-backend` feature: `cargo build -p axon-inference --features
# python --features candle-backend`. If not compiled, `__new__` returns
# `InferenceError("Candle backend not compiled: ...")`.
try:
    engine_candle = InferenceEngine(ModelConfig(
        path="model.safetensors", backend=InferenceBackend.Candle, device=Device.cpu(),
        input_shape=(1, 64, 128), output_dim=3,
    ))
except Exception as e:  # candle-backend feature not enabled
    print(f"skip: {e}")
```

#### `InferenceError` exception system

`InferenceError` **directly** inherits builtin `Exception` (a `PyException` subclass), **not** the Stage 1 `AxonError` base class. Reason: same as `BacktestError` / `RiskError` / `OmsError` / `ExchangeError` — `axon-inference` reverse-depending on `axon-python::AxonError` would create a cargo cycle, so the Rust side does not hard-depend on it. Error code is taken from the Rust `Debug` output variant name (e.g. `ModelNotFound` / `ModelLoadFailed` / `Onnx(...)` / `Candle(...)`), stable across releases.

```python
from axon_quant.inference import InferenceEngine, ModelConfig, InferenceError, InferenceBackend, Device

cfg = ModelConfig(
    path="/nonexistent.onnx", backend=InferenceBackend.Onnx, device=Device.cpu(),
    input_shape=(1, 64, 128), output_dim=3,
)

try:
    engine = InferenceEngine(cfg)
    engine.load("/nonexistent.onnx")
except InferenceError as e:
    # e.args[0] is the stable error code (e.g. "ModelNotFound")
    # e.args[1] is the human-readable form: "[ModelNotFound] model file not found: /nonexistent.onnx"
    print(e.args[0], e.args[1])
```

**Caveats / Stage 6 limitations**:

- `Tch` backend is **not** exposed to Python (avoids PyTorch C++ linking); `InferenceEngine(InferenceBackend.Tch)` returns `InferenceError("Tch backend is not exposed to Python in Stage 6 ...")`.
- `ModelHotReloader.__new__` returns `RuntimeError` because `PyInferenceEngine` does not expose the underlying `ModelConfig` (waiting for Stage 7+ to add an internal accessor). Use `engine.infer_batch([...])` for batch inference in the meantime.
- `BatchInferencePipeline` is a simplified Python wrapper (no tokio `batch_loop` task). It buffers `Observation`s in a `Vec` and calls `engine.infer_batch` on `collect()`, which already runs `par_iter` (rayon) internally.
- `Extension-module` PyO3 feature is **disabled** by default (would break `cargo test` static linking). Build the wheel via `make python-develop` instead of `cargo build --features python` to get the actual cdylib.

### axon_quant.compliance (Stage 7 delivery) — Compliance & Audit Engine

PyO3 bindings for `axon-compliance`. Provides trade recording, immutable audit log (blockchain-style hash chain), report generation, and regulator submission. Stage 7 exposes the full Rust compliance module to Python, with internal state guarded by `Mutex` for thread safety.

| Class / Function | Purpose |
| --- | --- |
| `ComplianceConfig` | Compliance configuration: `account_id` / `base_currency` / `large_trade_threshold` / `position_limit` / `max_portfolio_concentration` / `data_retention_years` / `regulators` |
| `ComplianceModule` | Main module: `record_trade(dict)` / `trade_count` / `audit_event_count` / `query_trades(filter)` / `get_trade_stats` / `generate_daily_report` / `generate_monthly_report` / `generate_annual_report` / `verify_audit_integrity` / `storage_path` / `config` |
| `TradeSide` | Enum: `Buy` / `Sell` (`__str__` returns `buy` / `sell`) |
| `OrderType` | Enum: `Market` / `Limit` / `StopLoss` / `TakeProfit` / `StopLimit` / `TrailingStop` (`__str__` returns `market` / `limit` / `stop_loss` / `take_profit` / `stop_limit` / `trailing_stop`) |
| `LiquidityType` | Enum: `Maker` / `Taker` |
| `TradeStatus` | Enum: `Pending` / `Filled` / `PartiallyFilled` / `Cancelled` / `Rejected` |
| `AuditEventType` | 17 audit event types: `TradeExecuted` / `OrderPlaced` / `OrderCancelled` / `OrderModified` / `PositionOpened` / `PositionClosed` / `StrategyStarted` / `StrategyStopped` / `ConfigChanged` / `UserLogin` / `UserLogout` / `ApiKeyCreated` / `ApiKeyRevoked` / `ReportGenerated` / `DataExported` / `SystemError` / `ComplianceAlert` |
| `TradeRecord` | Helper: `required_fields()` / `optional_fields()` static methods returning required / optional field names for `record_trade` dict (`__new__` not constructible; use dict protocol) |
| `load_config_from_toml(path, storage_path=None)` | One-step factory: load config from TOML file, create `ComplianceModule` (Stage 1 compat entry) |

#### Example: Basic compliance flow

```python
import tempfile
from axon_quant.compliance import ComplianceModule, ComplianceConfig

tmp = tempfile.mkdtemp()
cfg = ComplianceConfig(
    account_id="acc-001",
    base_currency="USDT",
    large_trade_threshold=100_000.0,
    position_limit=1_000_000.0,
    max_portfolio_concentration=0.4,
    data_retention_years=7,
    regulators=["SEC", "FINRA"],
)
cm = ComplianceModule(cfg, tmp)

# Record trade (dict protocol; enum strings are case-insensitive)
cm.record_trade({
    "strategy_id": "strat-1",
    "symbol": "BTCUSDT",
    "side": "buy",
    "quantity": 1.0,
    "price": 50_000.0,
    "fee": 50.0,
    "fee_currency": "USDT",
    "exchange": "Binance",
})

print(cm.trade_count, cm.audit_event_count)  # 1 1
print(cm.verify_audit_integrity())  # True
```

#### record_trade dict protocol

`record_trade(dict)` accepts a dict (lowering the bar — Python users do not need to import 5 enums). Fields:

| Field | Required | Type | Notes |
| --- | --- | --- | --- |
| `strategy_id` | ✓ | str | Strategy ID |
| `symbol` | ✓ | str | Trading pair (e.g. `BTCUSDT`) |
| `side` | ✓ | str | `buy` / `sell` (case-insensitive) |
| `quantity` | ✓ | float | Quantity, > 0 |
| `price` | ✓ | float | Price, > 0 |
| `fee` | ✓ | float | Fee amount |
| `fee_currency` | ✓ | str | Fee currency |
| `exchange` | ✓ | str | Exchange name |
| `trade_id` | ✗ | str (UUID) | Auto-generated if absent |
| `order_id` | ✗ | str (UUID) | Auto-generated if absent |
| `execution_time` | ✗ | str (RFC3339) | Defaults to current UTC |
| `settlement_time` | ✗ | str (RFC3339) | None by default |
| `status` | ✗ | str | `pending` / `filled` / `partially_filled` / `cancelled` / `rejected` (default `filled`) |
| `order_type` | ✗ | str | `market` / `limit` / `stop_loss` / `take_profit` / `stop_limit` / `trailing_stop` (default `market`) |
| `exchange_trade_id` | ✗ | str | Exchange-returned trade ID |
| `liquidity` | ✗ | str | `maker` / `taker` (default `taker`) |
| `realized_pnl` | ✗ | float | Realized PnL |
| `funding_rate` | ✗ | float | Funding rate |
| `slippage` | ✗ | float | Slippage |

Errors:
- `KeyError` — missing required field
- `ValueError` — wrong type / UUID parse failure / invalid status string
- `ComplianceError` — quantity/price ≤ 0 / notional mismatch / audit failure

#### Query & stats

```python
# Query trades (all filters optional)
btc_trades = cm.query_trades({
    "symbol": "BTCUSDT",
    "side": "buy",
    "min_notional": 10_000.0,
    "start_time": "2026-01-01T00:00:00Z",
    "end_time": "2026-12-31T23:59:59Z",
})

# Stats (dict return)
stats = cm.get_trade_stats("2026-01-01T00:00:00Z", "2026-12-31T23:59:59Z")
print(stats["total_trades"], stats["win_rate"], stats["avg_trade_size"])
```

#### Report generation (daily / monthly / annual)

```python
# Daily report (date="YYYY-MM-DD", starting_balance)
daily = cm.generate_daily_report("2026-06-24", 100_000.0)
print(daily["account_id"], daily["net_pnl"])

# Monthly report (year, month)
monthly = cm.generate_monthly_report(2026, 6)

# Annual report (year, initial_balance)
annual = cm.generate_annual_report(2026, 100_000.0)
```

#### `ComplianceError` exception system

`ComplianceError` **directly** inherits builtin `Exception` (`PyException` subclass), **not** Stage 1 `AxonError` base class. Reason: same as `BacktestError` / `RiskError` / `OmsError` / `ExchangeError` / `InferenceError` — `axon-compliance` would create a cargo cycle if it depended on `axon-python::AxonError`, so the Rust side has no hard dependency.

Error codes are stable across releases (taken from Rust `Debug` output variant names):

| Code | Trigger |
| --- | --- |
| `InvalidTradeData` | quantity / price ≤ 0, notional mismatch |
| `ConcentrationLimitBreached` | position concentration over limit |
| `LargeTradeThresholdExceeded` | single trade exceeds large-trade threshold |
| `AuditIntegrityFailed` | audit log hash chain verification failed |
| `StorageError` | file storage failure |
| `SerializationError` | serialization / deserialization failure |
| `ReportError` | report generation failure |
| `RegulatorFormatError` | regulator submission format failure |
| `ConfigError` | config parse / validation failure |

```python
from axon_quant.compliance import ComplianceModule, ComplianceConfig, ComplianceError
import tempfile

cfg = ComplianceConfig(
    account_id="acc-001", base_currency="USDT",
    large_trade_threshold=100_000.0, position_limit=1_000_000.0,
    max_portfolio_concentration=0.4, data_retention_years=7, regulators=["SEC"],
)
cm = ComplianceModule(cfg, tempfile.mkdtemp())

try:
    cm.record_trade({
        "strategy_id": "x", "symbol": "BTCUSDT", "side": "buy",
        "quantity": -1.0,  # triggers InvalidTradeData
        "price": 50_000.0, "fee": 50.0, "fee_currency": "USDT", "exchange": "Binance",
    })
except ComplianceError as e:
    # e.args[0] is the stable error code (e.g. "InvalidTradeData")
    # e.args[1] is the human-readable form: "[InvalidTradeData] Invalid trade data: Quantity must be positive"
    print(e.args[0], e.args[1])
```

**Caveats / Stage 7 limitations**:

- `ComplianceModule` uses `Mutex<RustModule>` internally for Python multi-thread safety (no lock-degradation risk).
- `query_trades` / `get_trade_stats` / `generate_*_report` are all **synchronous** (no async), CPU-bound, no `block_on` wrapper needed.
- Report dicts are produced via `serde_json` round-trip from the Rust `DailyReport` / `MonthlyReport` / `AnnualReport` structs — **no** corresponding pyclass on the Python side (avoids 30+ field boilerplate).
- `TradeRecord` is **not** exposed as a constructible pyclass; Python side uses dict protocol (`required_fields()` / `optional_fields()` only provide metadata).
- `AuditEvent` is **not** exposed to Python (only `audit_event_count` getter), internal fields are chain-managed by `AuditLog`.
- `load_config_from_toml(path, storage_path=None)` is a Stage 1 compat entry; recommended new API is `ComplianceModule(cfg, storage_path)`.

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

### axon_quant.risk (Stage 3 delivery)

Pre-trade risk engine, with 8 risk thresholds + standalone circuit breaker + risk metrics aggregation + portfolio monitoring alerts.

| Class | Description |
|-------|-------------|
| `DefaultRiskEngine` | Main risk engine: `check_order` / `check_portfolio` / `update_daily_pnl` / `reset_daily` / `metrics` |
| `RiskConfig` | 8 risk threshold configuration (position per instrument / total exposure / order value / leverage / drawdown / daily loss / concentration / circuit breaker cooldown) |
| `CircuitBreaker` | Standalone circuit breaker: `check_and_trigger` / `reset` / `is_active` (does not depend on `DefaultRiskEngine`) |
| `RiskMetrics` | Risk metrics aggregation (NAV / leverage / drawdown / daily PnL / VaR(95) / concentration) |
| `RiskResult` | Check result (Allow / Reject(reason) / Warn(msg)), uses `kind` tag pattern (not PyO3 enum) |
| `RiskReason` | Reject reason (8 variants flattened): `OrderTooLarge` / `PositionLimitExceeded` / `MaxLeverageExceeded` / `MaxDrawdownExceeded` / `DailyPnLLimit` / `CircuitBreakerActive` / `ConcentrationTooHigh` / `InsufficientMargin` |
| `RiskError` | Risk exception (inherits `Exception`, **not** `AxonError`, to avoid cargo cycles) |
| `make_order(...)` | Factory function, returns order dict (limit / market) |
| `make_portfolio(...)` | Factory function, returns minimal portfolio dict (only base_currency / commission_rate) |
| `make_portfolio_with_positions(...)` | Factory function, returns portfolio dict with cash + positions |
| `make_risk_config(...)` | Factory function, returns `RiskConfig` instance |
| `make_circuit_breaker(...)` | Factory function, returns `CircuitBreaker` instance |

#### Example: pre-trade risk check + circuit breaker + risk metrics

```python
from axon_quant.risk import (
    DefaultRiskEngine, RiskConfig, CircuitBreaker,
    RiskResult, RiskReason, RiskMetrics, RiskError,
    make_order, make_portfolio, make_portfolio_with_positions,
    make_risk_config, make_circuit_breaker,
)

# 1) Create risk engine
engine = DefaultRiskEngine(make_risk_config(
    max_order_value=10_000.0,     # max order value
    max_leverage=2.0,              # max leverage multiplier
    max_daily_loss=5_000.0,        # max daily loss (triggers circuit breaker)
    max_concentration=0.30,        # max concentration of single instrument
))

# 2) Construct order + portfolio
order = make_order(
    id=1, symbol="BTC-USDT", side="Buy",
    type="limit", price=100.0, quantity=1.0,
)
portfolio = make_portfolio(
    base_currency="USD",
    commission_rate=0.001,
    cash={"USD": 100_000.0},
)

# 3) Pre-trade check
result = engine.check_order(order, portfolio)
if result.is_allow:
    print("Order allowed")
elif result.is_reject:
    reason = result.reason
    print(f"Rejected: {reason.kind}")  # e.g. "OrderTooLarge"
else:
    print(f"Warning: {result.message}")
```

#### Cumulative daily PnL triggers circuit breaker

```python
# Cumulative daily loss exceeds threshold → engine.check_order() rejects
engine.update_daily_pnl(2_000.0)    # cumulative profit
engine.update_daily_pnl(-7_500.0)   # cumulative loss exceeds 5_000 → tripped
r = engine.check_order(order, portfolio)
assert r.is_reject and r.reason.kind == "CircuitBreakerActive"

# Reset daily state (does not reset VaR history window)
engine.reset_daily()
```

#### RiskReason field access

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

#### Standalone CircuitBreaker (does not depend on engine)

```python
cb = make_circuit_breaker(daily_loss_limit=10_000.0, cooldown_seconds=3600)
cb.check_and_trigger(-5_000.0)   # not at threshold, inactive
assert cb.is_active is False
cb.check_and_trigger(-15_000.0)  # triggered
assert cb.is_active is True
cb.reset()                       # force reset
assert cb.is_active is False
```

#### `RiskError` exception system

`RiskError` **directly** inherits builtin `Exception` (a `PyException` subclass), **not** the Stage 1 `AxonError` base class. Reason: `axon-risk` reverse-depending on `axon-python::AxonError` would create a cargo cycle (`axon-python` depends on `axon-risk`), so the Rust side does not hard-depend on it.

```python
try:
    engine.check_order(bad_order, portfolio)
except axon_quant.risk.RiskError as e:  # actually Exception subclass
    code = e.args[0]   # e.g. "OrderRejected" / "CircuitBreakerActive"
    msg = e.args[1]    # e.g. "[OrderRejected] Order too large: ..."
```

| Error Code | Meaning |
|------------|---------|
| `CircuitBreakerActive` | Circuit breaker is active, order rejected |
| `OrderRejected` | Order rejected by risk check |
| `ConfigInvalid` | Risk config is invalid |
| `Overflow` | Numeric overflow |

### axon_quant.oms (Stage 4 delivery)

Order Management System (OMS), covering the full order lifecycle (submit / cancel / state machine), fill event handling, multi-currency cash + multi-symbol positions, and idempotency key deduplication.

| Class / Factory | Description |
|----------------|-------------|
| `OrderManager` | Main OMS class: `submit` / `cancel` / `update_status` / `get_order_status` / `batch_submit` / `add_fill` / `snapshot` / `snapshot_balance` / `snapshot_positions` / `active_count` / `history_count` / `deposit` |
| `Order` | Order object: `symbol` / `side` / `order_type` / `quantity` / `price` / `idempotency_key` |
| `OrderStatus` | Order status (`kind` tag pattern): `New` / `Acknowledged` / `PartiallyFilled(filled_qty, avg_price)` / `Filled` / `Cancelled` / `Rejected(reason)` / `Expired`, with `is_terminal()` predicate |
| `Side` | Enum: `Buy` / `Sell` |
| `OrderType` | Enum: `Limit` / `Market` |
| `Portfolio` | Multi-currency cash + positions container: `deposit` / `apply_fill` / `cash` / `positions` / `position_count` / `is_empty` / `to_dict` |
| `Position` | Single-symbol position: `symbol` / `quantity` / `avg_price` / `realized_pnl` / `updated_at` / `to_dict` |
| `OmsError` | OMS exception (inherits `Exception`, **not** `AxonError`, to avoid cargo cycles) |
| `limit_order(symbol, side, quantity, price, idempotency_key=None)` | Factory returning a limit `Order` |
| `market_order(symbol, side, quantity, idempotency_key=None)` | Factory returning a market `Order` (taker price to be confirmed by matching) |
| `make_order_status(kind, filled_qty=None, avg_price=None, reason=None)` | Factory constructing an `OrderStatus` from a dict |

#### Example: full order lifecycle + portfolio update

```python
from axon_quant.oms import (
    OrderManager, Order, OrderStatus, Side, OrderType, Portfolio, Position,
    OmsError, limit_order, market_order, make_order_status,
)

# 1) Create OMS + initial funding
mgr = OrderManager()
mgr.deposit("USDT", 100_000)

# 2) Submit order → returns order_id (UUID 36 chars)
oid = mgr.submit(limit_order(
    "BTC-USDT", "Buy", quantity=1, price=50_000,
    idempotency_key="my-bot-001",
))
print(oid, mgr.active_count())   # 1

# 3) Advance state machine
mgr.update_status(oid, make_order_status("Acknowledged"))

# 4) Push fill event (partial fill) → portfolio auto-updated
mgr.add_fill(
    order_id=oid, fill_id="f1", symbol="BTC-USDT",
    price=50_000, quantity=0.6, fee=0,
)
s = mgr.get_order_status(oid)
assert s.kind == "PartiallyFilled"
assert s.filled_qty == "0.6"

# 5) Query portfolio
snap = mgr.snapshot_balance()
assert snap["cash"]["USDT"] == "70000.0"
pos = snap["positions"]["BTC-USDT"]
assert pos.quantity == "0.6"
assert pos.avg_price == "50000"

# 6) Push completing fill → terminal state
mgr.add_fill(
    order_id=oid, fill_id="f2", symbol="BTC-USDT",
    price=51_000, quantity=0.4, fee=0,
)
assert mgr.get_order_status(oid) is None  # Filled removed from active
```

#### Batch submission + idempotency key

```python
# Idempotency key dedup (re-submit with same key raises DuplicateIdempotencyKey)
oid_a = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="batch-1"))
try:
    oid_a2 = mgr.submit(limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="batch-1"))
except OmsError as e:
    assert e.args[0] == "DuplicateIdempotencyKey"

# Batch submit (loops submit, throws first error on failure)
oids = mgr.batch_submit([
    limit_order("ETH-USDT", "Buy", 1, 3_000, idempotency_key="batch-2"),
    limit_order("SOL-USDT", "Buy", 10, 100, idempotency_key="batch-3"),
])
assert len(oids) == 2
```

#### OrderStatus field access

```python
status = make_order_status("PartiallyFilled", filled_qty=0.6, avg_price=50_000)
assert status.kind == "PartiallyFilled"
assert status.filled_qty == "0.6"
assert status.avg_price == "50000"
assert status.is_terminal() is False

# Terminal predicate
filled = make_order_status("Filled", filled_qty=1, avg_price=50_000)
assert filled.is_terminal() is True
cancelled = make_order_status("Cancelled", reason="user_cancelled")
assert cancelled.is_terminal() is True
```

#### Standalone Portfolio class

```python
# Lightweight portfolio not depending on OrderManager (testing / serialization scenarios)
p = Portfolio()
p.deposit("USDT", 100_000)
p.deposit("BTC", 1.5)
p.apply_fill(
    fill_id="f1", symbol="BTC-USDT",
    price=50_000, quantity=0.6, fee=0,
)
assert p.cash["USDT"] == "70000.0"
assert p.positions["BTC-USDT"].quantity == "0.6"
assert p.position_count() == 1

# Serialize
d = p.to_dict()  # {"cash": {...}, "positions": {...}, "position_count": 1}
```

#### Decimal bridge (lossless precision)

All monetary fields (`quantity` / `price` / `filled_qty` / `avg_price` / `realized_pnl` / `cash` dict values) are returned to Python as **strings**, constructed via `decimal.Decimal`. Reason: `rust_decimal::Decimal` carries 128-bit precision and cannot be safely cast to `float`; string round-trip is zero-loss.

```python
from decimal import Decimal

o = limit_order("BTC-USDT", "Buy", Decimal("0.1"), Decimal("50000.5"))
# quantity / price are also Decimal strings on the Python side
```

#### `OmsError` exception system

`OmsError` **directly** inherits builtin `Exception` (a `PyException` subclass), **not** the Stage 1 `AxonError` base class. Reason: `axon-oms` reverse-depending on `axon-python::AxonError` would create a cargo cycle (`axon-python` depends on `axon-oms`), so the Rust side does not hard-depend on it.

```python
try:
    mgr.cancel("not-a-uuid")
except axon_quant.oms.OmsError as e:  # actually Exception subclass
    code = e.args[0]    # e.g. "OrderNotFound"
    msg = e.args[1]     # e.g. "[OrderNotFound] order not found: xxx"
```

| Error Code | Meaning |
|------------|---------|
| `OrderNotFound` | Order ID not found |
| `InvalidTransition` | Illegal state machine transition (e.g. Filled → PartiallyFilled) |
| `DuplicateIdempotencyKey` | Duplicate idempotency key |
| `AlreadyTerminal` | Operating on a terminal order (Filled / Cancelled / Rejected) |
| `ExchangeRejected` | Exchange rejected the order |
| `NetworkError` | Network failure |
| `SerializationError` | Serialization failure |
| `RecoveryFailed` | State recovery failed |
| `Portfolio` | Portfolio error (fill qty inconsistent with cash, etc.) |

### `axon_quant.exchange` Submodule (Stage 5) — Real Exchange Adapters

Real exchange adapters (Binance, OKX) with WebSocket subscription, rate limiting, order lifecycle management, and circuit breaker. **Testnet enabled by default**; production mode requires explicit configuration. API keys are read from environment variables (`BINANCE_API_KEY` / `BINANCE_API_SECRET` / `OKX_API_KEY` / `OKX_API_SECRET` / `OKX_PASSPHRASE`).

| Class / Function | Description |
|------------------|-------------|
| `ExchangeId` | Enum: `Binance` / `Okx` |
| `ExchangeConfig` | Full exchange configuration (`api_key` / `api_secret` / `passphrase` / `rest_base_url` / `ws_url` / `testnet` / `rate_limit` / `reconnect`) |
| `RateLimitConfig` | Token-bucket rate limit (RPS / orders per minute / WS messages per second) |
| `ReconnectConfig` | Auto-reconnect + circuit breaker configuration (max_retries / backoff / threshold) |
| `BinanceAdapter` | Binance adapter (REST + WebSocket, testnet / production) |
| `OkxAdapter` | OKX adapter (REST + WebSocket, testnet / production, requires `passphrase`) |
| `OrderLifecycleManager` | Order state machine tracking (Pending → Acknowledged → Filled / Rejected / Cancelled) |
| `TokenBucketRateLimiter` | Token-bucket rate limiter (synchronous `try_acquire` + status read) |
| `ExchangeError` | Exchange-specific error (inherits `Exception`, **not** `AxonError` to avoid cargo cycle) |
| `binance_testnet_config()` | Factory: read Binance testnet API keys from environment variables |
| `okx_testnet_config()` | Factory: read OKX testnet API keys from environment variables |

#### Example: testnet connection (env keys)

```python
import os
from axon_quant.exchange import BinanceAdapter, binance_testnet_config

# API key 自动从环境变量读取(BINANCE_API_KEY / BINANCE_API_SECRET)
# 缺一即抛 ExchangeError("BINANCE_API_KEY / BINANCE_API_SECRET not set in environment")
os.environ["BINANCE_API_KEY"] = "..."
os.environ["BINANCE_API_SECRET"] = "..."

adapter = BinanceAdapter(binance_testnet_config())
adapter.connect()  # 同步包装:内部 block_on 异步 connect
adapter.subscribe(symbols=["BTCUSDT"], kind="ticker")

# 下单:接受 dict,返回 order_id (UUID 字符串)
oid = adapter.place_order({
    "symbol": "BTCUSDT",
    "side": "buy",
    "type": "market",
    "quantity": "0.001",
    "tif": "IOC",
})

# 撤单
adapter.cancel_order(oid)

# 查询
balances = adapter.get_balance()
positions = adapter.get_positions()
```

#### Order dict protocol

Order construction uses Python dicts (no need to construct axon-oms types directly):

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `symbol` | str | ✓ | Trading pair (Binance: `"BTCUSDT"`; OKX: `"BTC-USDT"`) |
| `side` | str | ✓ | `"buy"` / `"sell"` |
| `type` | str | ✓ | `"market"` / `"limit"` / `"stop_loss"` / `"stop_limit"` |
| `quantity` | str / Decimal | ✓ | Order quantity (lossless string transfer) |
| `tif` | str | ✓ | `"GTC"` / `"IOC"` / `"FOK"` |
| `price` | str / Decimal | (limit types) | Limit price |
| `client_order_id` | str | optional | Client order ID (UUID string; auto-generated if missing) |
| `meta` | dict | optional | Exchange-specific metadata (e.g. Binance `newClientOrderId`) |

#### `ExchangeError` exception system

`ExchangeError` **directly** inherits builtin `Exception` (a `PyException` subclass), **not** the Stage 1 `AxonError` base class. Reason: same as `BacktestError` / `RiskError` / `OmsError` — `axon-exchange` reverse-depending on `axon-python::AxonError` would create a cargo cycle. Error code is taken from the variant name, stable across releases.

```python
from axon_quant.exchange import OrderLifecycleManager, ExchangeError

mgr = OrderLifecycleManager()
try:
    mgr.update_status(
        "00000000-0000-0000-0000-000000000000",
        {"status": "filled", "filled_qty": "0.1", "avg_price": "50000"},
    )
except ExchangeError as e:
    code = e.args[0]   # e.g. "OrderNotFound"
    msg  = e.args[1]   # e.g. "[OrderNotFound] order not found: ..."
```

| Error Code | Meaning |
|------------|---------|
| `ConnectionFailed` | REST / WebSocket connection failure |
| `WebSocketDisconnected` | WebSocket unexpectedly dropped |
| `AuthenticationFailed` | API key signature verification failed |
| `OrderRejected` | Exchange rejected the order (min notional, etc.) |
| `InsufficientBalance` | Balance insufficient |
| `RateLimited` | API rate limit triggered (returns `wait_ms`) |
| `OrderNotFound` | Order ID not found |
| `ParseError` | Response parsing failure |
| `ApiError` | Generic API error (with `code` + `message` fields) |
| `WebSocket` | WebSocket error message |
| `CircuitBreakerOpen` | Circuit breaker open (consecutive failures exceeded threshold) |
| `Network` | Network failure |
| `Serialization` | (de)serialization failure |

#### Security: API keys are never exposed

`api_secret` / `passphrase` are **never** serialized to `__repr__`, never printed, never logged. Verify with `repr(adapter)` or `repr(config)`:

```python
adapter = BinanceAdapter(binance_testnet_config())
print(repr(adapter))   # "BinanceAdapter(...)"  — no secret
print(repr(config))    # "ExchangeConfig(Binance, testnet=True, rest=...)"  — no secret
```

See `docs/en/reference/exchange-security.md` for the full security checklist.

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

## Agent Swarm Multi-Agent Collaboration

axon_quant supports multi-Agent collaboration framework using Actor model for professional division and voting consensus.

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    SwarmOrchestrator                          │
│  - Agent lifecycle management                                │
│  - Message routing                                           │
│  - Voting coordination                                       │
└────────────────────┬────────────────────────────────────────┘
                     │ tokio::mpsc
         ┌───────────┼───────────┐
         ▼           ▼           ▼
    ┌──────────┐ ┌──────────┐ ┌──────────┐
    │ Market   │ │ Risk     │ │ Execution│
    │ Agent    │ │ Agent    │ │ Agent    │
    └──────────┘ └──────────┘ └──────────┘
```

### Core Components

| Component | Description |
|-----------|-------------|
| `AgentId` | Unique Agent identifier |
| `AgentRole` | Agent role (Market / Risk / Execution / Audit) |
| `AgentMessage` | Inter-Agent message |
| `MessageContent` | Message content (MarketSignal / RiskSignal / TradeOrder, etc.) |
| `VoteProposal` | Voting proposal |
| `VoteResult` | Voting result |
| `ConsensusManager` | Consensus manager |
| `SwarmOrchestrator` | Swarm orchestrator |

### Usage Example

```python
# Agent Swarm is currently implemented only in Rust layer
# Python bindings will be provided in future versions
```

### Design Document

For detailed design, refer to [Agent Swarm Architecture Design](https://github.com/pengwow/axon_quant/blob/main/.axon-internal/specs/2026-06-21-agent-swarm-design.md).

## DeFi On-Chain Trading (Experimental)

> **Note**: DeFi features are experimental and under active development. APIs may change.

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              axon-defi                                  │
│                                                                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐ │
│  │ EvmAdapter   │  │ UniswapV3    │  │ MevShare     │  │ BridgeMgr  │ │
│  │ (Exchange    │  │ Router       │  │ Client       │  │ (LayerZero)│ │
│  │  Adapter)    │  │              │  │              │  │            │ │
│  └──────────────┘  └──────────────┘  └──────────────┘  └────────────┘ │
│  ┌──────────────┐                                                     │
│  │ ContractRisk │                                                     │
│  │ Checker      │                                                     │
│  └──────────────┘                                                     │
└─────────────────────────────────────────────────────────────────────────┘
```

### Core Components

| Component | Description |
|-----------|-------------|
| `EvmAdapter` | EVM chain adapter, implements ExchangeAdapter trait |
| `UniswapRouter` | Uniswap V3 router, optimal path execution |
| `MevShareClient` | MEV-Share client, sandwich attack prevention |
| `ContractRiskChecker` | Smart contract risk checker |
| `BridgeManager` | Cross-chain bridge manager, LayerZero integration |

### Supported Chains

| Chain | Chain ID | LayerZero ID |
|-------|----------|--------------|
| Ethereum | 1 | 101 |
| Arbitrum | 42161 | 110 |
| Optimism | 10 | 111 |
| Polygon | 137 | 109 |

### Python Types

| Type | Description |
|------|-------------|
| `Chain` | EVM chain enum (Ethereum / Arbitrum / Optimism / Polygon) |
| `EvmConfig` | EVM chain config (RPC, private key, API key) |
| `DefiOrder` | DeFi order (token, amount, slippage) |
| `SwapRoute` | Swap route (input/output token, fee) |
| `RiskCheckResult` | Risk check result |
| `UniswapV3Contracts` | Uniswap V3 contract addresses |
| `DefiError` | DeFi exception |

### Usage Example

```python
from axon_quant._native.defi import (
    Chain, EvmConfig, DefiOrder, SwapRoute, RiskCheckResult,
    UniswapV3Contracts, DefiError,
)

# 1. Get chain config
chain = Chain.Ethereum
print(f"Chain: {chain.name}, ID: {chain.chain_id}")

# 2. Get Uniswap V3 contract addresses
contracts = UniswapV3Contracts.for_chain(Chain.Ethereum)
print(f"Router: {contracts.router}")

# 3. Create EVM config
config = EvmConfig(
    chain_id=1,
    rpc_url="https://mainnet.infura.io/v3/xxx",
    private_key="0x...",
)

# 4. Create DeFi order
order = DefiOrder("0xtoken", "1000", 50000.0)
print(f"Order: {order}")
```

### Design Document

For detailed design, refer to [DeFi On-Chain Trading Architecture Design](https://github.com/pengwow/axon_quant/blob/main/.axon-internal/specs/2026-06-21-defi-onchain-trading-design.md).

## Next Steps

- [API Reference](api-reference.md) — Complete API documentation
- [Configuration](configuration.md) — Configuration options
- [Quick Start](../getting-started/quickstart.md) — Get started with Python
