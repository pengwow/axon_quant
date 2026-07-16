# AXON Module Reference

> This document describes every crate in the workspace **crate-by-crate**: its responsibility, mechanism, applicable/non-applicable scenarios, code location and usage.
> Use it as the index for questions like "Which crate should I use?" or "Where is this feature implemented?".

## Reading Conventions

Each module section uses 7 standardized fields:

| Field | Meaning |
|-------|---------|
| **Core Responsibility** | One-sentence summary of what the module does and which problem it solves |
| **Code Location** | Key file/directory paths (relative to the repo root) |
| **Core Mechanism** | Key internal implementation principles (data structures, algorithms, concurrency model) |
| **Applicable Scenarios** | Business/engineering scenarios where you should use this module |
| **Non-applicable Scenarios** | Scenarios where this module is not needed (to prevent misuse) |
| **How to Use** | Minimal code example (**Python first, Rust as the underlying reference**) |
| **Key Dependencies** | Upstream dependents + downstream dependencies |

---

## 1. `axon-core`

### Core Responsibility
The **bottom-most** type library in the workspace; defines shared data structures, error conventions, scheduling primitives and statistical primitives. **Does not depend on** any other `axon-*` crate.

### Code Location
- `crates/axon-core/src/lib.rs` — public re-exports
- `crates/axon-core/src/time/` — `Timestamp` / `MonotonicClock` / `TimePrecision`
- `crates/axon-core/src/types/` — `Price` / `Quantity` / `Symbol`
- `crates/axon-core/src/market/` — `Tick` / `Bar` / `OrderBookSnapshot` / `Trade`
- `crates/axon-core/src/order/` — `Order` / `OrderType` / `TimeInForce` / `OrderStatus` state machine
- `crates/axon-core/src/event/` — `Event` / `EventBuilder` / `EventRouter` / `EventHandler`
- `crates/axon-core/src/queue/` — `EventQueue` (priority queue sorted by timestamp)
- `crates/axon-core/src/portfolio/` — multi-currency `Portfolio` / `Position` / `TradeRecord`
- `crates/axon-core/src/scheduler/` — simulated clock + scheduled/periodic tasks
- `crates/axon-core/src/impact/` — Linear / Power-law / Adaptive / Almgren-Chriss market impact models
- `crates/axon-core/src/latency/` — Constant / Normal / Exponential / Uniform / Queueing latency models
- `crates/axon-core/src/volatility/` — EWMA / Rolling / Garman-Klass volatility
- `crates/axon-core/src/fee/` — tiered fee schedule + Maker/Taker billing
- `crates/axon-core/src/metrics/` — trading metric aggregation
- `crates/axon-core/src/simd/` — SIMD-accelerated normalization / VaR / orderbook
- `crates/axon-core/src/harness_types.rs` — `AgentIntent` / `TaskContext` / `HarnessResult`

### Core Mechanism
- **Zero-dependency**: no new crates beyond what is explicitly declared in `Cargo.toml`
- **`#[repr(C)]` + compact layout**: e.g. `Tick` is 32 bytes (i64 + 2 × f64 + 1-byte side + padding), ready for SIMD loads
- **BinaryHeap + timestamp**: event queue is a min-heap sorted by `(timestamp, seq)`
- **State machine**: `OrderStatus` transitions are guarded by `matches!` in `order/status.rs`
- **serde-compatible**: all cross-boundary data is serializable

### Applicable Scenarios
- Reuse `EventQueue` + `Order` when building custom matchers, backtests or microstructure simulations
- Use the 4 `ImpactModel` implementations in the `impact` module when researching market impact and slippage
- Use `Order` / `Trade` / `TradeRecord` when passing orders and fills across processes / languages
- Use the `latency` module to inject random delays for latency-sensitivity experiments

### Non-applicable Scenarios
- High-frequency market data ingestion (use `axon-exchange` / `axon-data`; they depend on this module but do not call it directly)
- Business strategy implementation (use `axon-rl` / `axon-llm` / `axon-ensemble`; do not manipulate `EventQueue` directly)
- Deployments that do not need to extend the type system (just consume `axon-core` types; do not reinvent `Order` in the business layer)

### How to Use

> `axon-core` is a **Rust internal foundation library**; it is not exposed as a Python binding directly. Python users access `Tick` / `Order` / `EventQueue` and other types indirectly through upper-level modules such as `axon-data` / `axon-backtest` / `axon-oms` (via the dict protocol / dataclass mapping). When direct access to internal Rust types is needed, the caller is usually the crate itself or integration tests.

**Python side (most common path: indirect use via `axon_quant.data.Tick` / `axon_quant.backtest`):**

```python
from axon_quant.data import DataService, DataRequest, Frequency, MockSource
from axon_quant.backtest import limit_order, BacktestEngine

# 1) Fetch Tick list via axon-data (internally axon_core::market::Tick)
svc = DataService.new().register_source(
    MockSource.with_tick_series("btc", 1000, 1_000_000, lambda i: 100.0 + i)
)
ds = svc.load(DataRequest("BTCUSDT", "2026-01-01T00:00:00Z",
                          "2026-01-02T00:00:00Z", Frequency.Min1))

# 2) Build an Order via axon-backtest (internally axon_core::order::Order)
bt = BacktestEngine(initial_cash=100_000.0)
bt.push_event({
    "type": "order_submitted",
    "timestamp_ns": 1_000,
    "order": limit_order(1, "BTCUSDT", "Buy", 100.0, 1.0),  # -> axon_core::order::Order::limit
})
result = bt.run()
```

**Rust side (use when developing a new crate / writing integration tests):**

```rust
use axon_core::{EventQueue, Order, OrderType, Side, Price, Quantity, Tick, Timestamp};

let mut q = EventQueue::new();
q.push(Tick::new(Timestamp::from_nanos(1_000_000_000), 50_000.0, 0.1, Side::Buy));

let order = Order::limit("BTC-USDT".into(), Side::Buy, Price::from(50_000.0), Quantity::from(0.01));
```

### Key Dependencies
- **Depended on by**: nearly all `axon-*` crates (`axon-backtest` / `axon-rl` / `axon-oms` / `axon-risk` …)
- **Depends on**: no `axon-*` crates (this is a hard design constraint)

---

## 2. `axon-backtest`

### Core Responsibility
Event-driven backtest engine + L1/L2/L3 deterministic matching + impact-aware matching + streaming backtest.

### Code Location
- `crates/axon-backtest/src/lib.rs` — public API (`BacktestEngine` / `L1MatchingEngine` / `L2MatchingEngine`)
- `crates/axon-backtest/src/engine.rs` — main loop (event → match → fill → portfolio)
- `crates/axon-backtest/src/matching/l1.rs` — price-time priority Level 1
- `crates/axon-backtest/src/matching/l2.rs` — multi-level price Level 2 with amend
- `crates/axon-backtest/src/matching/l3/` — Level 3: call auction, dark pool, orderbook snapshot/restore
- `crates/axon-backtest/src/impact/` — `ImpactedMatchingEngine` (wraps base matcher with `ImpactModel`)
- `crates/axon-backtest/src/streaming/` — streaming backtest: `StreamingStrategy::on_tick` / `StrategyAction` / `ExchangeStreamSource` / `ReplayStreamSource`
- `crates/axon-backtest/src/python/` — PyO3 bindings (`axon_quant.backtest`)
- `crates/axon-backtest/tests/` — 17 e2e integration tests

### Core Mechanism
- **Matching algorithms**:
  - L1 (default): price-time priority queue at the same price level
  - L2: keeps multi-level price depth, supports order amend
  - L3: includes call auction, dark pool, orderbook snapshot/restore
- **Determinism**: single-threaded event loop, no concurrent side effects; same input always yields the same output
- **Impact injection**: `ImpactedMatchingEngine` wraps the base matcher and adjusts fill price by `ImpactModel::compute_impact(quantity, ...)`
- **Streaming backtest**: strategy implements `StreamingStrategy::on_tick(&Tick, &OrderBook) -> StrategyAction`; `StreamingEngine` drives the loop; `ExchangeStreamSource` uses `crossbeam::channel`, `ReplayStreamSource` replays from CSV / Vec

### Applicable Scenarios
- Any strategy research that needs reproducible backtests (start with `BacktestEngine` + `L2MatchingEngine`)
- Validating out-of-sample performance of RL policies (`axon-rl::TradingEnv` wraps this module)
- Simulating real fill slippage (`ImpactedMatchingEngine` + `LinearImpactModel`)
- Running tick-level high-frequency strategies and bridging to live data via the streaming pipeline (`streaming::engine.rs`)

### Non-applicable Scenarios
- Real order placement (use `axon-oms` + `axon-exchange`)
- Sub-millisecond microstructure backtests of a single instrument (L3 matching is an engineering approximation, not exchange-level simulation)
- Cross-instrument portfolio optimization (that is the domain of `axon-ensemble` / `axon-hpo`)

### How to Use

**Python side (primary usage; covers the vast majority of strategy research scenarios):**

```python
from axon_quant.backtest import (
    BacktestEngine, L2MatchingEngine, ImpactedMatchingEngine,
    ImpactedMatchingEngineBuilder, limit_order, market_order,
)

# 1) Event-driven backtest: L1 (default) / L2 matching
bt = BacktestEngine(initial_cash=100_000.0)
bt.with_matching_engine(L2MatchingEngine())           # optional: switch to L2
bt.with_seed_liquidity(half_spread=0.5, depth_levels=10, size_per_level=1.0)
bt.begin_bar(price=50_000.0, symbol="BTCUSDT")        # required per bar
bt.push_event({
    "type": "order_submitted",
    "timestamp_ns": 1_000_000_000,
    "order": limit_order(1, "BTCUSDT", "Buy", 50_000.0, 0.1),
})
result = bt.run()
print(result.final_nav, result.fills)

# 2) Realistic slippage simulation: layer an ImpactModel on top
ie = (ImpactedMatchingEngineBuilder()
      .model_type("linear")
      .coefficient(0.1)
      .depth_levels(5)
      .build())
ie.submit(limit_order(2, "BTCUSDT", "Buy", 50_000.0, 0.1))

# 3) You can also call the matcher directly (bypassing BacktestEngine)
l2 = L2MatchingEngine()
fill = l2.submit(limit_order(3, "BTCUSDT", "Sell", 50_000.0, 0.1))
print(fill["is_filled"], fill["fills"])
```

**Rust side (use when developing a new matching algorithm / tuning performance):**

```rust
use axon_backtest::{BacktestEngine, L2MatchingEngine};
use axon_core::Tick;

let mut engine = BacktestEngine::new(L2MatchingEngine::new("BTC-USDT".into()));
engine.feed_tick(Tick::new(/* ... */));
let result = engine.run_to_end()?;
println!("Sharpe = {}, MaxDD = {}", result.sharpe(), result.max_drawdown());
```

### Key Dependencies
- **Depends on**: `axon-core` (type foundation)
- **Depended on by**: `axon-rl` (trading environment wrapper), `axon-llm::trading` (backtest utilities), various `tests/` integration tests

---

## 3. `axon-rl`

### Core Responsibility
Gymnasium-compatible reinforcement learning trading environment + multi-objective reward + vectorized parallel rollout.

### Code Location
- `crates/axon-rl/src/lib.rs` — entry + re-exports
- `crates/axon-rl/src/env/trading_env.rs` — `TradingEnv` (Gymnasium 5-tuple interface)
- `crates/axon-rl/src/env/action_decoder.rs` — action → order conversion
- `crates/axon-rl/src/env/executor.rs` — uses `axon-backtest` to execute orders
- `crates/axon-rl/src/observation/` — feature engineering (sliding window / normalization / `BoxSpace` / `DiscreteActionSpace`)
- `crates/axon-rl/src/action/` — action spaces (Discrete / Continuous / MultiDiscrete) + converters + smoothers
- `crates/axon-rl/src/reward/` — reward functions (PnL / Sharpe / Sortino / MultiObjective / Scaled)
- `crates/axon-rl/src/vec_env/` — `SyncVecEnv` / `AsyncVecEnv` parallel rollout
- `crates/axon-rl/src/python/` — PyO3 bindings (`axon_quant.rl`)

### Core Mechanism
- **Environment state machine**: `TradingEnv` internally maintains `current_step / market_state / portfolio_state`; `step(action)` returns `(obs, reward, terminated, truncated, info)`
- **Action spaces**: `DiscreteActionConverter` maps int to `(side, qty_bin, type)`; `ContinuousActionConverter` maps `[-1, 1]` to `(direction, size_ratio)`
- **Reward functions**: `PnLReward` / `SharpeReward` / `SortinoReward` / `MultiObjectiveReward` (combinable with weights)
- **Parallel rollout**: `AsyncVecEnv` uses tokio + `crossbeam-channel` to dispatch steps; `SyncVecEnv` uses rayon

### Applicable Scenarios
- Training PPO / SAC / A2C RL agents (use `axon_quant.rl.TradingEnv` in Python and connect to Stable-Baselines3 / RLlib)
- Multi-objective RL training (use `MultiObjectiveReward` with HPO searching the weights)
- Running large-scale parallel rollout on CPU (`AsyncVecEnv` with `num_envs=64`)
- Running baseline policies (use `DiscreteActionSpace` with the default reward)

### Non-applicable Scenarios
- Online learning (the environment is offline; no live data feed)
- Ultra-low latency (one `TradingEnv` step includes 1 match + 1 portfolio update + 1 feature computation; microsecond level, not nanosecond)
- Rule-based strategies that do not need RL (use `axon-backtest::BacktestEngine` directly)

### How to Use

**Python side (primary usage; connects to Stable-Baselines3 / RLlib and other RL libraries):**

```python
import axon_quant
from stable_baselines3 import PPO

# 1) Create a trading environment (Gymnasium 5-tuple interface)
env = axon_quant.rl.TradingEnv(
    config={"initial_capital": 100_000.0, "max_steps": 500},
    market_data=bars,                                  # np.ndarray / list[dict]
    action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    reward="sharpe",                                   # "sharpe" / "sortino" / "pnl" / "multi_objective"
)

# 2) Standard RL training loop
obs, info = env.reset()
model = PPO("MlpPolicy", env, verbose=1)
model.learn(total_timesteps=10_000)

# 3) Inference / replay
obs, reward, terminated, truncated, info = env.step([0.5])
print(reward, info)  # info usually contains sharpe / drawdown / position_size

# 4) Vectorized parallel rollout (CPU multi-core acceleration)
venv = axon_quant.rl.AsyncVecEnv(                      # or SyncVecEnv
    num_envs=64,
    env_fn=lambda: axon_quant.rl.TradingEnv(config={...}, market_data=bars),
)
model = PPO("MlpPolicy", venv, verbose=1)
```

**Rust side (use when developing new rewards / new action spaces):**

```rust
use axon_rl::TradingEnv;
use axon_core::market::Bar;

// Implement your own FeaturePipeline and inject it into the environment
let env = TradingEnv::builder()
    .bars(bars)
    .reward(SharpeReward::new(window=63))
    .action_space(ContinuousActionSpace::new(-1.0, 1.0))
    .build();
let (obs, info) = env.reset();
let (obs, reward, term, trunc, info) = env.step(&vec![0.5]);
```

### Key Dependencies
- **Depends on**: `axon-core` (types) + `axon-backtest` (matching execution)
- **Depended on by**: `axon-distributed` (RLlib integration), `axon-llm` (LLM-driven RL decisions), `axon-tracker` (training metrics)

---

## 4. `axon-hpo`

### Core Responsibility
Hyperparameter optimization toolchain: search space definition + Optuna integration (Python side) + multi-objective + Pareto front + hypervolume.

### Code Location
- `crates/axon-hpo/src/lib.rs` — entry
- `crates/axon-hpo/src/config.rs` — `HPOConfig` / `SamplerConfig` / `PrunerConfig`
- `crates/axon-hpo/src/search_space.rs` — `SearchSpaceDef` (Uniform / LogUniform / Int / Categorical)
- `crates/axon-hpo/src/trial.rs` — `TrialResult` / `TrialState`
- `crates/axon-hpo/src/result.rs` — `HPOResult`
- `crates/axon-hpo/src/pareto.rs` — `ParetoFront` / `compute_hypervolume` / `dominates`
- `crates/axon-hpo/python/axon_hpo/` — Python-side Optuna adapter (`optuna_runner.py` / `multi_objective.py` / `pruning.py` / `search_space.py`)

### Core Mechanism
- **Search space**: `SearchSpaceDef` uses an enum to represent each distribution; serialized and passed to Optuna
- **Multi-objective Pareto**: `dominates(a, b)` implements Pareto dominance comparison; `compute_hypervolume` uses NSGA-II style WFG algorithm
- **Pruning**: `MedianPruner` / `SuccessiveHalvingPruner` early-terminate poorly-performing trials
- **Python bridge**: `python/axon_hpo/optuna_runner.py` translates the Rust search space into Optuna `suggest_*` calls

### Applicable Scenarios
- RL policy hyperparameter search (learning rate / discount factor / network architecture)
- Strategy parameter tuning (take-profit/stop-loss thresholds / position cap / signal window size)
- Multi-objective HPO (optimize Sharpe + MaxDD simultaneously)
- Combine with walk-forward (`axon-integration-tests` ships ready-made examples)

### Non-applicable Scenarios
- Large-model fine-tuning (this is HPO, not NAS; no GPU scheduling on the Rust side)
- Real-time adaptation (Optuna is offline batch; no online learning)
- Discrete decision-tree problems (use XGBoost's own tuner)

### How to Use

**Python side (primary usage; drive Optuna directly):**

```python
from axon_hpo import HPORunner, SearchSpace, StudyConfig
import axon_quant

# 1) Define the search space
space = (SearchSpace()
    .uniform("lr", 1e-5, 1e-2)
    .log_uniform("gamma", 0.9, 0.999)
    .categorical("activation", ["relu", "tanh"])
    .int_uniform("hidden_size", 64, 512, step=64))

# 2) Define the objective (train + evaluate with axon_quant.rl)
def objective(trial_params: dict) -> float:
    env = axon_quant.rl.TradingEnv(config={**trial_params, "max_steps": 500},
                                   market_data=bars,
                                   action_space={"type": "continuous",
                                                 "min": -1.0, "max": 1.0})
    # Simplified training: evaluate a random policy directly
    obs, _ = env.reset()
    sharpe = 0.0
    for _ in range(500):
        a = env.action_space.sample()
        _, r, term, trunc, info = env.step(a)
        sharpe = info.get("sharpe", 0.0)
        if term or trunc: break
    return sharpe

# 3) Run the search
study = HPORunner(study_config=StudyConfig(direction="maximize", n_trials=50))
best = study.run(space, objective_fn=objective)
print(best.params, best.value)

# 4) Multi-objective search (Pareto front)
pareto_study = HPORunner(study_config=StudyConfig(
    directions=["maximize", "minimize"], n_trials=100))  # first: sharpe, second: maxdd
```

**Rust side (use when developing new pruners / embedding into the training pipeline):**

```rust
use axon_hpo::{HPOConfig, SamplerConfig, PrunerConfig, SearchSpaceDef};

let cfg = HPOConfig {
    n_trials: 50,
    sampler: SamplerConfig::Tpe,
    pruner: PrunerConfig::Median { warmup_steps: 5 },
};
let space = SearchSpaceDef::new()
    .uniform("lr", 1e-5, 1e-2)
    .log_uniform("gamma", 0.9, 0.999);
```

### Key Dependencies
- **Depends on**: `axon-core`
- **Depended on by**: `axon-rl` (training loop), `axon-llm` (prompt tuning), `axon-integration-tests`

---

## 5. `axon-walk-forward`

### Core Responsibility
Time-series specific rolling / expanding window validation + purge / embargo leak prevention + OOS metrics aggregation + Deflated Sharpe Ratio.

### Code Location
- `crates/axon-walk-forward/src/lib.rs` — entry
- `crates/axon-walk-forward/src/config.rs` — `WalkForwardConfig` / `WindowType`
- `crates/axon-walk-forward/src/split.rs` — `TimeSeriesSplitter` (Rolling / Expanding)
- `crates/axon-walk-forward/src/purge.rs` — `purge_overlapping_labels` / `embargo_indices` / `detect_leakage`
- `crates/axon-walk-forward/src/metrics.rs` — `FoldResult` / `ISMetrics` / `OOSMetrics` / `StabilityMetrics`
- `crates/axon-walk-forward/src/evaluation.rs` — `aggregate_folds` / `compute_deflated_sharpe`

### Core Mechanism
- **Window splitting**: `TimeSeriesSplitter::split(start, end)` returns `Vec<FoldSplit>`; each contains train/val/test indices
- **Leak prevention**:
  - `purge_overlapping_labels` removes training samples whose labels overlap with the validation set
  - `embargo_indices` adds an empty buffer at the train/val boundary
  - `detect_leakage` reports the number of potentially leaked samples
- **Deflated Sharpe**: `compute_deflated_sharpe` corrects for multiple-testing bias (after Bailey & López de Prado)

### Applicable Scenarios
- Evaluate the true out-of-sample performance of a strategy
- Detect overfitting (Deflated Sharpe significantly lower than the plain Sharpe)
- Rolling retraining (e.g. retrain monthly using the prior 12 months)
- Compose with `axon-hpo` + `axon-registry` for a complete training pipeline (see `axon-integration-tests::e2e_pipeline`)

### Non-applicable Scenarios
- IID data (use sklearn's `KFold`; this module is designed specifically for time series)
- A single backtest (no notion of folds)
- Real-time streaming (no such capability)

### How to Use

**Python side (primary usage; pair with RL / HPO to evaluate real out-of-sample performance):**

```python
import axon_quant
from axon_quant.walk_forward import (
    WalkForwardConfig, WindowType, TimeSeriesSplitter,
    compute_deflated_sharpe, detect_leakage,
)
import numpy as np

# 1) Configure the rolling window
cfg = WalkForwardConfig(
    window_type=WindowType.Rolling,
    train_window=252,           # 1 trading year
    test_window=63,             # 3 months
    step=63,                    # step = test window
    embargo=5,                  # 5-day gap to prevent leakage
)

# 2) Generate fold splits
splitter = TimeSeriesSplitter(cfg)
folds = splitter.split(start="2023-01-01", end="2025-01-01")
print(f"Total folds: {len(folds)}")  # typically 9-10

# 3) Run a backtest per fold + collect OOS metrics
oos_returns = []
for fold in folds:
    train_bars = bars[fold.train_start:fold.train_end]
    test_bars  = bars[fold.test_start:fold.test_end]

    env = axon_quant.rl.TradingEnv(config={"initial_capital": 100_000},
                                   market_data=train_bars, reward="sharpe")
    # ... train the policy on train, evaluate on test ...
    oos_returns.append(fold_test_sharpe)

# 4) Leak detection + Deflated Sharpe
issues = detect_leakage(folds, bars)
print(f"Potentially leaked samples: {issues.count}")

deflated = compute_deflated_sharpe(
    sharpe_ratios=oos_returns, n_trials=50  # corrects for HPO multiple-testing bias
)
print(f"Deflated Sharpe = {deflated:.3f}")
# If significantly lower than the in-sample Sharpe, the strategy is overfit
```

**Rust side (use when developing new splitters / embedding into axon-integration-tests):**

```rust
use axon_walk_forward::{WalkForwardConfig, TimeSeriesSplitter, WindowType};

let cfg = WalkForwardConfig {
    window_type: WindowType::Rolling,
    train_window: 252,
    test_window: 63,
    step: 63,
    embargo: 5,
    ..Default::default()
};
let splits = TimeSeriesSplitter::new(cfg).split(start, end);
```

### Key Dependencies
- **Depends on**: `axon-core`
- **Depended on by**: `axon-rl` (eval pipeline), `axon-registry` (pick the best OOS version), integration tests

---

## 6. `axon-cli`

### Core Responsibility
The `axon` command-line entry point (Phase 0 only prints a banner; later stages will add backtest / train / run subcommands).

### Code Location
- `crates/axon-cli/src/main.rs`

### Core Mechanism
- Minimal `fn main() -> Result<()>` entry
- Uses compile-time constants like `env!("CARGO_PKG_VERSION")` / `env!("RUSTC_VERSION")` / `target_triple`

### Applicable Scenarios
- Current stage (0.4.0): verify build, version, platform info
- Future (plan 0.5+): unified entry for subcommands like `axon backtest run` / `axon train ppo` / `axon serve`

### Non-applicable Scenarios
- Real business invocation right now (features not yet implemented)
- Programmatic API (use `axon-python` bindings)

### How to Use

```bash
$ cargo run -p axon-cli
axon 0.4.0
Rust 1.97.0 (aarch64-apple-darwin)
Stage: Phase 0 — Architecture & Infrastructure
```

### Key Dependencies
- **Depends on**: `axon-core` (only the `Result` type)
- **Depended on by**: none yet

---

## 7. `axon-distributed`

### Core Responsibility
Ray / RLLib distributed training cluster configuration + Actor / ParamServer / Checkpoint fault tolerance.

### Code Location
- `crates/axon-distributed/src/lib.rs` — entry
- `crates/axon-distributed/src/config.rs` — `DistributedConfig` / `ClusterConfig` / `AlgorithmConfig` / `ResourceConfig` / `FaultToleranceConfig`
- `crates/axon-distributed/src/actor.rs` — `ActorConfig`
- `crates/axon-distributed/src/param_server.rs` — `ParamServerConfig`
- `crates/axon-distributed/src/checkpoint.rs` — `TrainingCheckpoint` / `StepMetrics` / `CheckpointMetadata`

### Core Mechanism
- **Config aggregation**: `DistributedConfig` aggregates 4 sub-configs; serialized to YAML and handed to Ray
- **Checkpoint chain**: `TrainingCheckpoint { step, metrics, model_state, optimizer_state }` serialized as Parquet/Arrow IPC, restorable by RLLib
- **Fault tolerance**: `FaultToleranceConfig` controls restart count / node-loss timeout

### Applicable Scenarios
- Multi-node multi-GPU PPO / SAC (scheduled via RLLib)
- Large-scale HPO (one actor per trial)
- Long training that needs checkpoint resumption
- `axon-integration-tests::distributed_flow` provides an end-to-end example

### Non-applicable Scenarios
- Single-node training (overhead exceeds the benefit)
- Strategy execution requiring nanosecond-level latency (network overhead is too large)
- Deploying inference services (that is the domain of `axon-inference`)

### How to Use

**Python side (primary usage; pair with Ray/RLlib to schedule multiple workers):**

```python
from axon_quant.distributed import (
    DistributedConfig, ClusterConfig, ResourceConfig,
    FaultToleranceConfig, AlgorithmConfig, to_yaml, from_yaml,
)

# 1) Build a multi-node multi-GPU cluster config
cfg = DistributedConfig(
    cluster=ClusterConfig(num_workers=8, num_gpus_per_worker=1),
    algorithm=AlgorithmConfig(name="PPO", framework="torch"),
    resources=ResourceConfig(memory_per_worker=16 * 1024**3),  # 16 GB
    fault_tolerance=FaultToleranceConfig(max_restarts=3, timeout_s=300),
)

# 2) Export to Ray / RLlib
yaml = to_yaml(cfg)
with open("cluster.yaml", "w") as f:
    f.write(yaml)

# 3) Pass the config dict directly into an RLlib trainer
# (axon-rl internally calls from_yaml for you)
from ray.rllib.algorithms.ppo import PPOConfig
ppo_cfg = PPOConfig().environment("axon-trading-env").framework("torch")
ppo_cfg.resources(num_gpus=1)

# 4) Checkpoint chain: save / restore during training
import axon_quant
ckpt = axon_quant.distributed.serialize_checkpoint(
    step=1000, metrics={"sharpe": 1.85}, model_state=model.state_dict())
axon_quant.distributed.save_checkpoint(ckpt, "/checkpoints/run1/step1000")
```

**Rust side (use when developing new fault-tolerance policies / custom actors):**

```rust
use axon_distributed::{DistributedConfig, ClusterConfig, ResourceConfig};

let cfg = DistributedConfig {
    cluster: ClusterConfig { num_workers: 8, num_gpus_per_worker: 1, .. },
    algorithm: Default::default(),
    resources: ResourceConfig { memory_per_worker: 16 * 1024 * 1024 * 1024, .. },
    fault_tolerance: Default::default(),
};
let yaml = serde_yaml::to_string(&cfg)?;
```

### Key Dependencies
- **Depends on**: `axon-core`
- **Depended on by**: `axon-integration-tests` (distributed_flow scenario), Python-side RLLib adapter

---

## 8. `axon-tracker`

### Core Responsibility
Unified trait + 4 backends for experiment tracking: `Memory` / `Local` / `MLflow` / `WandB`.

### Code Location
- `crates/axon-tracker/src/lib.rs` — entry
- `crates/axon-tracker/src/tracker.rs` — `ExperimentTracker` trait
- `crates/axon-tracker/src/types.rs` — `MetricEntry` / `ParamValue` / `ArtifactInfo` / `RunStatus`
- `crates/axon-tracker/src/backends/memory.rs` — `MemoryTracker` (for tests)
- `crates/axon-tracker/src/backends/local.rs` — `LocalTracker` (local JSONL)
- `crates/axon-tracker/src/backends/mlflow.rs` — `MlflowTracker` (`http` feature)
- `crates/axon-tracker/src/backends/wandb.rs` — `WandbTracker` (`http` feature)
- `crates/axon-tracker/src/retry.rs` — `RetryPolicy`

### Core Mechanism
- **Unified trait**: `ExperimentTracker::start_run / log_metric / log_param / log_artifact / end_run`
- **MetricBuffer**: `TrackerBackend` uses `MetricBuffer` to batch flushes and lower IO frequency
- **Retry**: `RetryPolicy { max_attempts, backoff }` wraps transient HTTP backend failures

### Applicable Scenarios
- Log per-episode reward / sharpe while training RL policies
- Log per-trial hyperparameters and final metrics during HPO
- Upload artifacts (model / report / config) to MLflow / W&B
- Use `MemoryTracker` to run unit tests without depending on external services

### Non-applicable Scenarios
- Real-time alerting in production (that is the domain of `axon-monitor`)
- Large-scale cross-run data analysis (query the MLflow tracking server directly)
- Business accounting that requires transactional consistency

### How to Use

**Python side (primary usage; switch between 4 backends as needed):**

```python
from axon_quant.tracker import (
    MemoryTracker, LocalTracker, MlflowTracker, WandbTracker,
)

# 1) Unit tests: MemoryTracker (no external service needed)
tracker = MemoryTracker()
run = tracker.start_run("ppo_btc_v1")
tracker.log_metric(run.id(), "sharpe", 1.85)
tracker.log_param(run.id(), "lr", 0.0003)
tracker.log_param(run.id(), "gamma", 0.99)
tracker.log_artifact(run.id(), "model.onnx")
tracker.end_run(run.id())

# 2) Local persistence: LocalTracker (writes JSONL)
local = LocalTracker(root_dir="/var/axon/runs")
run = local.start_run("ppo_btc_v1", tags={"env": "testnet"})

# 3) Remote: MLflow / W&B
mlf = MlflowTracker(tracking_uri="http://mlflow:5000", experiment="axon-rl")
run = mlf.start_run("ppo_btc_v1")
mlf.log_metric(run.id(), "sharpe", 1.85, step=1000)

wdb = WandbTracker(project="axon-rl", entity="my-team")
run = wdb.start_run("ppo_btc_v1", config={"lr": 0.0003})

# 4) Report per-step metrics while training with RLlib
for step in range(total_steps):
    # ... train ...
    tracker.log_metric(run.id(), "episode_reward", reward, step=step)
```

**Rust side (use when developing new backends / embedding into the training loop):**

```rust
use axon_tracker::{MemoryTracker, ExperimentTracker};

let mut tracker = MemoryTracker::new();
let run = tracker.start_run("ppo_btc_v1")?;
tracker.log_metric(run.id(), "sharpe", 1.85)?;
tracker.log_param(run.id(), "lr", 0.0003)?;
tracker.end_run(run.id())?;
```

### Key Dependencies
- **Depends on**: `axon-core`
- **Depended on by**: `axon-rl` (training), `axon-hpo` (trial logging), `axon-registry` (linking run ids)

---

## 9. `axon-registry`

### Core Responsibility
Model registry: version management + stage lifecycle (`staging` → `production`) + multi-backend storage + metadata signing.

### Code Location
- `crates/axon-registry/src/lib.rs` — entry
- `crates/axon-registry/src/registry.rs` — `ModelRegistry` main struct
- `crates/axon-registry/src/types.rs` — `ModelVersion` / `ModelStage` / `ModelMetadata` / `SemVer`
- `crates/axon-registry/src/signature.rs` — `ModelSignature` (input/output tensor description)
- `crates/axon-registry/src/storage.rs` — `LocalStorage` / `StorageBackend` trait
- `crates/axon-registry/src/filter.rs` — `VersionFilter`

### Core Mechanism
- **Three stages**: `None` → `Staging` → `Production` → `Archived`; every stage transition is audit-logged
- **Signature check**: `ModelSignature` records input shape / dtype / output dimension; loaders enforce a match
- **Local storage**: `LocalStorage` stores model files under `{root}/{model_name}/{version}/`, JSON metadata alongside

### Applicable Scenarios
- Train multiple checkpoints, pick the best one and promote to `production`
- A/B testing (keep both `production` and `challenger` simultaneously)
- Model rollback (archived versions can be restored in one step)
- `axon-integration-tests::walkforward_registry` demonstrates automatic registration after validation

### Non-applicable Scenarios
- Large-scale distributed file systems (should connect to S3 / OSS; see the storage-backend extension roadmap)
- Model training itself
- Inference serving (after loading, handled by `axon-inference`)

### How to Use

**Python side (primary usage; model version management + A/B canary):**

```python
from axon_quant.registry import (
    ModelRegistry, ModelStage, LocalStorage, ModelSignature,
)

# 1) Construct a local storage backend + registry
storage = LocalStorage(root_dir="/var/axon/models")
registry = ModelRegistry(storage=storage)

# 2) Register a new model version
version = registry.register_model(
    name="ppo_btc",
    version="1.0.0",
    model_bytes=open("model.onnx", "rb").read(),
    signature=ModelSignature(
        input_shape=(1, 64, 128),
        input_dtype="float32",
        output_dim=3,
    ),
    metadata={"sharpe": 1.85, "trained_on": "2026-06-01"},
)

# 3) Stage flow: None -> Staging -> Production
registry.promote("ppo_btc", "1.0.0", stage=ModelStage.Staging)
# ... run a few hours of shadow trading ...
registry.promote("ppo_btc", "1.0.0", stage=ModelStage.Production)

# 4) A/B: keep both production and challenger
registry.register_model("ppo_btc", "1.1.0", ...)        # new version as challenger
# axon-inference loads by stage
prod_path = registry.get_artifact_path("ppo_btc", stage=ModelStage.Production)
challenger_path = registry.get_artifact_path("ppo_btc", stage=ModelStage.Staging)

# 5) Rollback: re-promote an old version to production
registry.promote("ppo_btc", "0.9.0", stage=ModelStage.Production)

# 6) List all versions
versions = registry.list_versions("ppo_btc", stage=ModelStage.Archived)
```

**Rust side (use when developing new storage backends / embedding into CI):**

```rust
use axon_registry::{ModelRegistry, ModelStage, LocalStorage};

let storage = LocalStorage::new("/var/axon/models")?;
let mut registry = ModelRegistry::new(Box::new(storage));
let version = registry.register_model("ppo_btc", "v1.0.0", model_bytes, signature)?;
registry.promote(&version, ModelStage::Staging)?;
```

### Key Dependencies
- **Depends on**: `axon-core`
- **Depended on by**: `axon-inference` (load production model), `axon-tracker` (link runs), integration tests

---

## 10. `axon-llm`

### Core Responsibility
LLM agents: ReAct reasoning loop + Tool Calling + context window + three built-in tools (market / portfolio / order) + multi-agent Swarm.

### Code Location
- `crates/axon-llm/src/lib.rs` — entry
- `crates/axon-llm/src/react_agent.rs` — `ReActAgent` (Reasoning + Acting)
- `crates/axon-llm/src/declarative_agent.rs` — declarative Agent (YAML config)
- `crates/axon-llm/src/context.rs` — `ContextManager` (sliding window + summary compression)
- `crates/axon-llm/src/prompt.rs` — `PromptTemplate`
- `crates/axon-llm/src/tools.rs` — `Tool` trait + error types
- `crates/axon-llm/src/trading/` — trading tool set (`PlaceOrderTool` / `QueryPortfolioTool` / `CancelOrderTool` / `ReplaceOrderTool`) + `MockTradingBackend` / `PaperBackend` / `SafetyMode`
- `crates/axon-llm/src/swarm/` — multi-agent collaboration: `Orchestrator` / `MarketAgent` / `RiskAgent` / `AuditAgent` / `Vote` / `PaperTrading`
- `crates/axon-llm/src/backends/` — LLM backends: `OpenAICompat` / `Mock` / `Recording` / `Retry` / `Cost` / `Streaming`
- `crates/axon-llm/src/explain/` — decision explanation bridge (integrates with `axon-explain`)

### Core Mechanism
- **ReAct loop**: `ReActAgent::run(input)` loops: LLM returns Thought → Action → Observation → Thought again → … until `FinishReason`
- **Tool calling**: `Tool::call(args) -> Result<Value>`, argument validation inside the tool
- **Swarm voting**: `Vote { HardVote / SoftVote / WeightedVote }`, orchestrator collects decisions from multiple agents and merges
- **Backend retry**: `backends/retry.rs` implements exponential backoff + circuit breaker; `backends/cost.rs` accumulates token cost
- **Recording & replay**: `backends/recording.rs` records request/response for e2e tests

### Applicable Scenarios
- Use an LLM to interpret market state and place orders (`ReActAgent` + `PlaceOrderTool`)
- Multi-agent risk control (`MarketAgent` emits signals, `RiskAgent` validates, `AuditAgent` records)
- A/B testing LLM prompts (record with `backends::recording` and compare offline)
- Run an end-to-end demo without connecting to an exchange via `MockTradingBackend`

### Non-applicable Scenarios
- Nanosecond-latency automated trading (LLM inference takes 100ms+)
- Offline batch backtest (`axon-backtest` + `axon-rl` is a better fit)
- Production large-model fine-tuning (this is the inference / orchestration layer, not the training layer)

### How to Use

**Python side (primary usage; 3 scenarios):**

```python
from axon_quant.llm import (
    LLMConfig, make_backend, LLMMessage, LLMBackend, ReActAgent,
    PlaceOrderTool, QueryPortfolioTool, MockTradingBackend,
)
from axon_quant.llm.swarm import (
    SwarmOrchestrator, MarketAgent, RiskAgent, AuditAgent, VoteType,
)
import axon_quant

# ─── Scenario 1: single Agent + Tool Calling (most common) ─────────
cfg = LLMConfig(
    backends=[{
        "name": "primary",
        "base_url": "https://api.openai.com/v1",
        "api_key": "sk-...",
        "model": "gpt-4o-mini",
        "temperature": 0.3,
        "max_tokens": 2048,
    }],
    retry={"max_retries": 5, "initial_backoff_ms": 100},
)
backend = make_backend(cfg)
resp = backend.chat([LLMMessage("user", "What's the current BTC market like?")])
print(resp.content)

# ─── Scenario 2: ReAct + order tools (connects to axon-oms/axon-risk) ─
oms = axon_quant.oms.OrderManager()
oms.deposit("USDT", 100_000)

mock_backend = MockTradingBackend()
place_tool = PlaceOrderTool(backend=mock_backend, mode="dry_run",
                            risk={"max_order_notional": 100.0,
                                  "allowed_symbols": ["BTC-USDT"]})
agent = ReActAgent(backend=make_backend(cfg), tools=[place_tool])
result = agent.run("Is now a good entry for BTC? If yes, buy 0.1 BTC")

# ─── Scenario 3: multi-agent Swarm (production-grade multi-view decisions) ─
orchestrator = SwarmOrchestrator(
    agents=[
        MarketAgent(backend=make_backend(cfg)),   # emit signals
        RiskAgent(backend=make_backend(cfg)),     # risk validation
        AuditAgent(backend=make_backend(cfg)),    # audit log
    ],
    vote_type=VoteType.SoftVote,                  # soft voting
)
decision = orchestrator.run({"symbol": "BTC-USDT", "market": bars[-100:]})
print(decision.action, decision.confidence, decision.votes)
```

**Rust side (use when developing new Tools / new backends):**

```rust
use axon_llm::{ReActAgent, MockBackend, PlaceOrderTool, QueryPortfolioTool};

let backend = MockBackend::new();
let agent = ReActAgent::builder(backend)
    .tool(Box::new(PlaceOrderTool::new(paper_backend)))
    .tool(Box::new(QueryPortfolioTool::new(portfolio)))
    .build();
let response = agent.run("What is the current BTC market like? Should we add to the position?").await?;
```

### Key Dependencies
- **Depends on**: `axon-core` / `axon-backtest` (backtest utilities) / `axon-oms` (order placement) / `axon-explain` (explanation, optional)
- **Depended on by**: `axon-integration-tests` (e2e_react_loop_test / live_trading_e2e)

---

## 11. `axon-explain`

### Core Responsibility
Explainability: SHAP feature attribution + counterfactual explanation + decision report generation.

### Code Location
- `crates/axon-explain/src/lib.rs` — entry
- `crates/axon-explain/src/shap.rs` — KernelSHAP / TreeSHAP implementation
- `crates/axon-explain/src/counterfactual.rs` — counterfactual explanation (find the minimal change that flips the decision)
- `crates/axon-explain/src/report.rs` — decision report (HTML / JSON / Markdown)
- `crates/axon-explain/src/traits.rs` — `Explainer` / `CounterfactualSearch` trait
- `crates/axon-explain/src/python/` — PyO3 bindings

### Core Mechanism
- **KernelSHAP**: model-agnostic explainer based on weighted linear regression
- **Counterfactual**: starting from the current sample, perturb features minimally until the model output flips
- **Report**: `report::generate` assembles SHAP values + counterfactual + raw input into a readable document

### Applicable Scenarios
- Regulatory compliance: explain why the strategy placed an order at a specific moment (GDPR / MiFID II)
- Debugging models: which features contributed the most, is it overfit to noise
- Decision review: `axon-llm::swarm::AuditAgent` can call the explainer to produce explanations
- Research paper visualization

### Non-applicable Scenarios
- Feature selection at training time (use L1 / Mutual Information; SHAP is too slow)
- Real-time decision making (a single-sample KernelSHAP takes seconds to minutes)
- Black-box external APIs (no gradient access; can only explain a proxy model)

### How to Use

**Python side (primary usage; 3 core scenarios):**

```python
from axon_quant.explain import (
    KernelSHAP, CounterfactualConfig, ReportGenerator,
    ActionSnapshot, ActionAttribution, ContributionDirection,
)
import numpy as np

# ─── Scenario 1: KernelSHAP feature attribution (most common) ─────────
# Input: model / background data / sample to explain
explainer = KernelSHAP(model=my_ppo_policy, background_data=X_train[:100])
attributions: ActionAttribution = explainer.explain(X_test[0])

# Marginal contribution of each feature
for feat, attr in zip(feature_names, attributions.values):
    direction = "+" if attr.direction == ContributionDirection.Positive else "-"
    print(f"  {direction} {feat}: {attr.marginal_contribution:+.3f}")

# ─── Scenario 2: counterfactual explanation (minimal perturbation to flip) ─
cf_config = CounterfactualConfig(
    target_class="Sell",            # target action to flip to
    max_features_perturbed=3,       # perturb at most 3 features
    distance_metric="l1",
)
cf = explainer.counterfactual(
    instance=X_test[0],
    config=cf_config,
)
print("Original decision: Buy @ 50000")
print(f"Minimal change → {cf.target_class}: change {cf.changed_features} to {cf.new_values}")
# e.g. change rsi_14 to 75 → decision flips to Sell

# ─── Scenario 3: decision report (HTML / JSON / Markdown) ─────────
gen = ReportGenerator(template="regulatory")     # also supports "minimal" / "full"
snapshot = ActionSnapshot(
    timestamp_ns=1_700_000_000_000_000_000,
    model_id="ppo_btc@1.0.0",
    input=X_test[0],
    output={"action": "Buy", "quantity": 0.1, "price": 50000.0},
    attributions=attributions,
    counterfactual=cf,
)
report = gen.generate(snapshot, format="html")
with open("/var/axon/reports/decision_20231115.html", "w") as f:
    f.write(report)
```

**Rust side (use when developing new Explainers / embedding LLM Agent decision audit):**

```rust
use axon_explain::{KernelShap, Explainer};

let explainer = KernelShap::new(model, background_data)?;
let attributions = explainer.explain(&instance)?;
```

### Key Dependencies
- **Depends on**: `axon-core`
- **Depended on by**: `axon-llm` (explain feature bridge), `axon-compliance` (generate compliance reports)

---

## 12. `axon-ensemble`

### Core Responsibility
Combine multiple RL / rule-based policies to improve robustness. Provides voting / weighted / stacking strategies.

### Code Location
- `crates/axon-ensemble/src/lib.rs` — entry
- `crates/axon-ensemble/src/manager.rs` — `EnsembleManager` (register / unload / dispatch sub-policies)
- `crates/axon-ensemble/src/voting.rs` — `HardVote` / `SoftVote` / `WeightedVote`
- `crates/axon-ensemble/src/stacking.rs` — `StackingEnsemble` (meta-model second-stage learning)
- `crates/axon-ensemble/src/dynamic.rs` — `DynamicWeightedEnsemble` (dynamically adjust weights by recent performance)
- `crates/axon-ensemble/src/traits.rs` — `Ensemble` / `Policy` / `VotingStrategy` trait
- `crates/axon-ensemble/src/types.rs` — `Action` / `ActionProbabilities` / `ModelPerformance`

### Core Mechanism
- **Voting**:
  - `HardVote`: majority wins
  - `SoftVote`: average probabilities
  - `WeightedVote`: weighted by model weights
- **Stacking**: treat sub-model outputs as features, train a meta-model (logistic / simple MLP)
- **Dynamic weights**: `DynamicWeightedEnsemble` adjusts weights by recent N-step rewards; well-performing sub-models gain larger weights

### Applicable Scenarios
- Train multiple hyperparameter sets / different RL algorithms and combine them (typical 1-3% Sharpe boost)
- A/B canary rollout (use `DynamicWeightedEnsemble` to give the new policy a low-weight starting point)
- Multi-timeframe strategy fusion (5min + 1h + 1d three sub-models vote)
- `axon-integration-tests` provides an ensemble + walk-forward e2e example

### Non-applicable Scenarios
- Single-policy baseline (overhead exceeds the benefit)
- Homogeneous sub-models (5 PPOs with the same seed in an ensemble ≈ 1 PPO)
- Inference latency < 1ms (every decision requires N forward passes)

### How to Use

**Python side (primary usage; 3 ensemble strategies):**

```python
from axon_quant.ensemble import (
    EnsembleManager, EnsembleStrategy,
    HardVoteStrategy, SoftVoteStrategy, WeightedVoteStrategy,
    StackingEnsemble, MetaModel, Observation, ModelWeight,
)
import axon_quant

# ─── 1) Soft voting (most common) ────────────────────────────
mgr = EnsembleManager(strategy=SoftVoteStrategy())
mgr.add_policy(axon_quant.inference.create_onnx_engine("ppo_v1.onnx", ...),
               weight=1.0)
mgr.add_policy(axon_quant.inference.create_onnx_engine("ppo_v2.onnx", ...),
               weight=1.0)
mgr.add_policy(axon_quant.inference.create_onnx_engine("sac_v1.onnx", ...),
               weight=0.7)

obs = Observation(features=current_state, symbol="BTC-USDT")
action = mgr.decide(obs)
print(action.action_type, action.confidence)

# ─── 2) Hard voting (majority wins) ───────────────────────────
mgr = EnsembleManager(strategy=HardVoteStrategy())

# ─── 3) Weighted voting (per-model weights) ──────────────────────
mgr = EnsembleManager(
    strategy=WeightedVoteStrategy(weights={
        "ppo_v1": 1.0, "ppo_v2": 0.8, "sac_v1": 0.5,
    })
)

# ─── 4) Stacking: train a meta-model for second-stage learning ─────────
stack = StackingEnsemble(
    meta_model=MetaModel.MLP,
    n_folds=5,                  # K-fold cross-validation to prevent leakage
)
stack.add_base_model("ppo_v1", axon_quant.inference.create_onnx_engine("ppo_v1.onnx", ...))
stack.add_base_model("sac_v1", axon_quant.inference.create_onnx_engine("sac_v1.onnx", ...))
stack.fit(X_meta_train, y_meta_train)  # use OOS predictions as meta features
action = stack.predict(obs)

# ─── 5) Dynamic weights: auto-adjust by recent performance ─────────
# Well-performing sub-models gain larger weights (reward-weighted)
mgr.update_performance("ppo_v1", recent_reward=2.5)
mgr.update_performance("ppo_v2", recent_reward=1.8)
```

**Rust side (use when developing new voting strategies / embedding into training):**

```rust
use axon_ensemble::{EnsembleManager, WeightedVoteStrategy, Policy};

let mut mgr = EnsembleManager::new(Box::new(WeightedVoteStrategy::new()));
mgr.add_policy(Box::new(ppo_policy_a));
mgr.add_policy(Box::new(ppo_policy_b));
let action = mgr.decide(&observation);
```

### Key Dependencies
- **Depends on**: `axon-core`
- **Depended on by**: `axon-rl` (multi-policy training), `axon-llm` (multi-agent decision approximation)

---

## 13. `axon-data`

### Core Responsibility
Unified market data ingestion: Mock / CSV / Parquet / cache (mmap) + feature pipeline (normalization / sliding window).

### Code Location
- `crates/axon-data/src/lib.rs` — entry
- `crates/axon-data/src/sources/mock.rs` — `MockSource` (default on)
- `crates/axon-data/src/sources/csv.rs` — `CsvSource` (`csv-source` feature)
- `crates/axon-data/src/sources/parquet.rs` — `ParquetSource` (`parquet-source` feature)
- `crates/axon-data/src/cache/control.rs` — L1 cache (`CacheControl`)
- `crates/axon-data/src/cache/mmap.rs` — L2 mmap shared cache (`mmap-cache` feature)
- `crates/axon-data/src/cache/shared_memory.rs` — cross-process shared memory
- `crates/axon-data/src/pipeline.rs` — `FeaturePipeline` (`ZScoreNormalizer` / `FeatureMatrix`)
- `crates/axon-data/src/dataset.rs` — row-oriented `Dataset` abstraction
- `crates/axon-data/src/bar/` — bar aggregation
- `crates/axon-data/src/ipc/` — Arrow IPC
- `crates/axon-data/src/python/` — PyO3 bindings (`axon_quant.data`)

### Core Mechanism
- **Unified trait**: `DataSource::fetch(&DataRequest) -> Result<Dataset>`
- **L1 cache**: `CacheControl` maintains `HashMap<key, Arc<Dataset>>` + LRU
- **L2 mmap**: `mmap-cache` feature uses `memmap2` to mmap Parquet, zero-copy across processes
- **Feature pipeline**: `FeaturePipeline` chains `ZScoreNormalizer` / `MinMax` / `Robust` to produce the training `FeatureMatrix`
- **Bar aggregation**: `bar::BarDataset` aggregates ticks into 1m / 5m / 1h candles

### Applicable Scenarios
- Connect historical CSV / Parquet for backtesting
- Share L2 mmap cache when running HPO (multiple trials read the same data zero-copy)
- Use `MockSource` to write unit tests
- Aggregate raw ticks into candles and feed the RL environment

### Non-applicable Scenarios
- Real-time market data ingestion (that is the domain of `axon-exchange`)
- Complex factor computation (use a dedicated feature store)
- Cross-machine caching (current mmap is single-node only)

### How to Use

**Python side (primary usage; 4 categories of data sources + cache):**

```python
from axon_quant.data import (
    DataService, DataRequest, Frequency, MockSource, CsvSource, ParquetSource,
    CacheControl, AxonError, DataError,
)
import datetime
import pyarrow as pa

# ─── 1) Mock data (preferred for unit tests) ──────────────────────────
svc = DataService.new().register_source(
    MockSource.with_tick_series("btc", 1000, 1_000_000, lambda i: 100.0 + i)
)
req = DataRequest("BTCUSDT", "2026-01-01T00:00:00Z",
                  "2026-01-02T00:00:00Z", Frequency.Min1)
ds = svc.load(req)
print(ds.len)        # 1000
batch = ds.to_arrow(0)  # pyarrow.RecordBatch (zero-copy)

# ─── 2) CSV / Parquet (production data ingestion) ──────────────────────────
svc = (DataService.new()
       .register_source(CsvSource(root_dir="/data/csv", tz="UTC"))
       .register_source(ParquetSource(root_dir="/data/parquet")))

req = DataRequest("BTCUSDT",
                  datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc),
                  datetime.datetime(2026, 6, 1, tzinfo=datetime.timezone.utc),
                  Frequency.Min1)
ds = svc.load(req)

# ─── 3) L1 cache (on by default; reused across requests) ───────────────────────
cache = CacheControl(max_entries=128, ttl_seconds=600)
svc = DataService.new().with_cache(cache).register_source(...)

# ─── 4) Pair with backtest / RL env (most common consumption path) ─────────────────
from axon_quant.backtest import BacktestEngine
from axon_quant.rl import TradingEnv

# Use DataService to fetch data → convert to numpy for backtest
bars_array = ds.to_numpy()  # shape: (n_bars, n_features)
env = TradingEnv(config={"initial_capital": 100_000},
                 market_data=bars_array, reward="sharpe")
```

**Cargo feature enablement (needed for CSV / Parquet / mmap):**

```toml
[dependencies]
axon-data = { path = "../axon-data",
              features = ["csv-source", "parquet-source", "mmap-cache"] }
```

**Rust side (use when implementing a custom DataSource):**

```rust
use axon_data::{MockSource, DataSource, DataRequest, Frequency};

let src = MockSource::new();
let dataset = src.fetch(&DataRequest::bars("BTC-USDT", Frequency::Min1, 1000))?;
```

### Key Dependencies
- **Depends on**: `axon-core`, arrow / parquet
- **Depended on by**: `axon-backtest` (historical data replay), `axon-rl` (observation data source), `axon-inference` (batch inference data)

---

## 14. `axon-compliance`

### Core Responsibility
Financial trading compliance auditing: trade records + blockchain-style audit log (hash chain, tamper-proof) + report generation + regulatory submission.

### Code Location
- `crates/axon-compliance/src/lib.rs` — `ComplianceModule` main struct
- `crates/axon-compliance/src/audit/log.rs` — `AuditLog` (hash chain append)
- `crates/axon-compliance/src/audit/storage.rs` — `FileStorage` (per-day file persistence)
- `crates/axon-compliance/src/regulator/metrics.rs` — regulatory metrics (concentration / large trade / position limit)
- `crates/axon-compliance/src/regulator/submission.rs` — regulatory submission generation + export
- `crates/axon-compliance/src/report/daily.rs` / `monthly.rs` / `annual.rs` / `formatter.rs` — daily / monthly / annual reports
- `crates/axon-compliance/src/types.rs` — `TradeRecord` / `TradeStatus` / `AuditEvent` / `ComplianceConfig`
- `crates/axon-compliance/src/python/` — PyO3 bindings

### Core Mechanism
- **Hash chain audit**: in `AuditLog`, each `AuditEvent`'s `event_hash = sha256(prev_hash || event_payload)`; verification recomputes the entire chain
- **Large trade alert**: when `TradeRecord.notional_value > large_trade_threshold`, log `tracing::warn!` but **does not block** the trade
- **Report generator**: `daily::DailyReportGenerator` / `monthly::MonthlyReportGenerator` / `annual::AnnualReportGenerator` each implement fixed fields
- **Export**: `ReportExporter::export(report, format)` supports JSON / CSV / custom formats

### Applicable Scenarios
- Compliance retention for real accounts (MiFID II / SEC / CSRC require 7-year retention)
- Internal audit: prove "this decision was generated by strategy X at time Y"
- Regulatory submission: generate `RegulatorySubmission` monthly and submit to the designated regulator
- Verify hash chain integrity in unit tests

### Non-applicable Scenarios
- Real-time risk blocking (`large_trade_threshold` only warns; to block, use `axon-risk`)
- Trade execution path (should be invoked asynchronously after `axon-oms`; should not block order placement)
- Large-scale historical data queries (use a dedicated OLAP store)

### How to Use

**Python side (primary usage; from config to report, end-to-end):**

```python
from axon_quant.compliance import (
    ComplianceModule, ComplianceConfig, load_config_from_toml,
    TradeSide, OrderType, LiquidityType, TradeStatus,
    ComplianceError,
)
from decimal import Decimal

# 1) Build the config (recommended: load from TOML)
cfg = ComplianceConfig(
    account_id="ACC-001",
    base_currency="USDT",
    large_trade_threshold=100_000.0,           # large-trade alert threshold
    position_limit=1_000_000.0,                # per-symbol cap
    max_portfolio_concentration=0.4,           # max concentration
    data_retention_years=7,                    # MiFID II / CSRC requirement
    regulators=["SEC", "CSRC"],
)
# Or load from file: cfg = load_config_from_toml("compliance.toml")

# 2) Start the module (internally uses Blake3/SHA256 hash chain persisted under audit_dir)
cm = ComplianceModule(cfg, audit_dir="/var/axon/audit")

# 3) Record a trade
cm.record_trade({
    "trade_id": "T-2026-0715-0001",
    "strategy_id": "ppo_btc@1.0.0",
    "symbol": "BTCUSDT",
    "side": TradeSide.Buy,
    "order_type": OrderType.Limit,
    "liquidity": LiquidityType.Taker,
    "quantity": Decimal("0.1"),
    "price": Decimal("50000.0"),
    "notional_value": Decimal("5000.0"),
    "fee": Decimal("5.0"),
    "status": TradeStatus.Filled,
    "venue": "binance",
    "executed_at_ns": 1_700_000_000_000_000_000,
})

# 4) Verify hash chain integrity (run after crash recovery)
assert cm.verify_audit_integrity(), "Audit chain has been tampered with!"

# 5) Generate daily / monthly / annual reports
daily = cm.generate_report(period="daily", date="2026-07-15")
cm.export_report(daily, format="json", path="/var/axon/reports/daily_0715.json")

# 6) Generate regulatory submission files
submission = cm.generate_regulatory_submission(regulator="SEC", period="monthly")
cm.export_report(submission, format="csv", path="/var/axon/submissions/sec_2026-07.csv")

# 7) Real-time large-trade alert (subscribe)
def on_large_trade(trade):
    print(f"⚠️ Large trade: {trade.symbol} {trade.notional_value} {trade.side}")
cm.subscribe_large_trade(threshold=50_000.0, callback=on_large_trade)
```

**Rust side (use when developing new report templates / embedding into oms async audit logging):**

```rust
use axon_compliance::{ComplianceModule, ComplianceConfig, TradeRecord, TradeSide, TradeStatus, OrderType, LiquidityType};

let cfg = ComplianceConfig {
    account_id: "test".into(),
    base_currency: "USDT".into(),
    large_trade_threshold: 100_000.0,
    position_limit: 1_000_000.0,
    max_portfolio_concentration: 0.4,
    data_retention_years: 7,
    regulators: vec!["SEC".into()],
};
let mut cm = ComplianceModule::new(cfg, "/tmp/audit")?;
cm.record_trade(TradeRecord { /* ... */ })?;
assert!(cm.verify_audit_integrity());
```

### Key Dependencies
- **Depends on**: `axon-core`, `chrono` / `uuid` / `sha2`
- **Depended on by**: `axon-oms` (async audit logging), `axon-risk` (linked alerting)

---

## 15. `axon-risk`

### Core Responsibility
**Pre-trade** risk control: order size / position / leverage / drawdown checks + circuit breaker (auto-pause on consecutive losses) + portfolio monitoring + VaR.

### Code Location
- `crates/axon-risk/src/lib.rs` — entry
- `crates/axon-risk/src/engine.rs` — `DefaultRiskEngine` (`RiskEngine` trait implementation)
- `crates/axon-risk/src/checks.rs` — various check functions
- `crates/axon-risk/src/circuit_breaker.rs` — `CircuitBreaker` (AtomicU8 state machine)
- `crates/axon-risk/src/config.rs` — `RiskConfig` (thresholds / windows / limits)
- `crates/axon-risk/src/metrics.rs` — `RiskMetrics` (real-time metrics)
- `crates/axon-risk/src/handler.rs` — `RiskEventHandler` (event-driven)
- `crates/axon-risk/src/python/` — PyO3 bindings

### Core Mechanism
- **Check chain** (typical 12ns total overhead):
  - Circuit breaker (AtomicBool, ~5ns, returns immediately if inactive)
  - Order size (~10ns)
  - Position limit (~50ns, HashMap lookup)
  - Leverage (~20ns)
  - Drawdown (~20ns)
- **Circuit breaker**: N consecutive losses → state `Closed → Open`; after cooldown, `HalfOpen` probe; success returns to `Closed`
- **VaR**: historical simulation, using the most recent N days' return distribution to estimate 95% / 99% quantile loss

### Applicable Scenarios
- Mandatory gate before live order placement (`axon-oms` calls `engine.check_order` before submit)
- Monitor portfolio drawdown, trigger circuit breaker to pause the strategy
- Real-time VaR / leverage / concentration calculation for the risk dashboard
- `axon-harness::HarnessBridge` also uses this module for strategy-level gating

### Non-applicable Scenarios
- Backtesting stage (the backtest itself uses historical data; mandatory risk control would distort results; use `ImpactedMatchingEngine` if you want to simulate)
- Low-latency high-frequency (12ns is the limit of a lock-free design; lower requires FPGA)
- Complex compliance retention (that is `axon-compliance`)

### How to Use

**Python side (primary usage; pre-trade gate + circuit breaker):**

```python
from axon_quant.risk import (
    DefaultRiskEngine, RiskConfig, CircuitBreaker,
    RiskResult, RiskReason, RiskMetrics, RiskError,
    make_order, make_portfolio, make_portfolio_with_positions,
    make_risk_config,
)
import axon_quant

# 1) Build the risk config
cfg = make_risk_config(
    max_order_value=10_000.0,           # max notional value per order
    max_position_per_symbol=100.0,      # max position per symbol
    max_total_exposure=1_000_000.0,     # total exposure cap
    max_leverage=3.0,                   # leverage cap
    max_drawdown=0.20,                  # max drawdown 20%
    max_daily_loss=5_000.0,             # daily loss circuit-breaker threshold
    max_concentration=0.4,              # single-symbol weight cap
    circuit_breaker_cooldown_s=300,     # circuit-breaker cooldown 5 minutes
)
engine = DefaultRiskEngine(cfg)

# 2) Pre-trade gate (required for every order submission)
order = make_order(symbol="BTC-USDT", side="Buy", type="limit",
                   price=50_000.0, quantity=0.1)
portfolio = make_portfolio(base_currency="USDT",
                           cash={"USDT": 100_000.0})
result = engine.check_order(order, portfolio)
if result.is_allow:
    axon_quant.oms.OrderManager().submit(order)
elif result.is_reject:
    log.warning(f"Risk rejected: {result.reason}")  # RiskReason enum

# 3) Circuit breaker
breaker = CircuitBreaker(
    max_consecutive_losses=5,           # trigger after 5 consecutive losing trades
    cooldown_seconds=300,
)
if breaker.check_and_trigger(recent_pnl=-200.0):
    # ... pause the strategy and wait for cooldown ...
    pass

# 4) Cumulative intraday PnL triggers the circuit breaker
engine.update_daily_pnl(-1_500.0)
# The next order is automatically rejected because intraday loss exceeds threshold
assert not engine.check_order(order, portfolio).is_allow
engine.reset_daily()  # reset at 0:00 every day

# 5) Real-time risk metrics
m: RiskMetrics = engine.metrics(portfolio)
print(f"NAV={m.nav}  leverage={m.leverage:.2f}  "
      f"drawdown={m.drawdown:.2%}  VaR95={m.var_95:.2f}")
```

**Rust side (use when developing new checks / embedding into oms pre-check):**

```rust
use axon_risk::{DefaultRiskEngine, RiskEngine, RiskResult};

let engine = DefaultRiskEngine::new(RiskConfig::default());
match engine.check_order(&order, &portfolio) {
    RiskResult::Allow => oms.submit(order)?,
    RiskResult::Reject(reason) => log::warn!("rejected: {:?}", reason),
    RiskResult::Warn(msg) => { oms.submit(order)?; log::warn!("warning: {}", msg); }
}
```

### Key Dependencies
- **Depends on**: `axon-core` / `axon-oms` (order types)
- **Depended on by**: `axon-exchange` (live order gate), `axon-oms` (pre-check), `axon-harness`

---

## 16. `axon-inference`

### Core Responsibility
Inference engine: ONNX / tch / Candle three backends + CPU/CUDA/Metal multi-device + batch inference pipeline + model hot-reload + **CPU affinity pinning**.

### Code Location
- `crates/axon-inference/src/lib.rs` — entry
- `crates/axon-inference/src/engine.rs` — `InferenceEngine` single-model inference
- `crates/axon-inference/src/backend/candle.rs` — Candle backend (pure Rust)
- `crates/axon-inference/src/backend/onnx.rs` — ONNX Runtime backend
- `crates/axon-inference/src/backend/tch.rs` — tch-rs (PyTorch C++) backend
- `crates/axon-inference/src/pipeline/batch.rs` — `BatchInferencePipeline` (tokio + rayon)
- `crates/axon-inference/src/pipeline/collector.rs` — request collector
- `crates/axon-inference/src/hot_reload.rs` — `ModelHotReloader` (notify file monitoring)
- `crates/axon-inference/src/affinity.rs` — **CPU/GPU thread affinity** (independent submodule)
- `crates/axon-inference/src/python/` — PyO3 bindings

### Core Mechanism
- **Backend trait**: `Backend::load / infer / warmup`
- **Batch inference**: `BatchInferencePipeline::submit(obs)` pushes into a bounded channel; the collector aggregates within a window and triggers `infer_batch`
- **Hot reload**: `notify` watches `model_path` changes → load new model → atomically replace `ArcSwap`
- **CPU affinity** (example):
  - **Core value**: ensure stable P99 latency for the batch inference pipeline; reduce cache thrashing when multiple models run concurrently
  - **Platform support**: Linux (`sched_setaffinity`) + macOS (`thread_policy_set`, MPS-aware); Windows runtime refuses (use WSL2 / numactl)
  - **GPU affinity**: CUDA uses `with_cuda(device_id)` to trigger `cudaSetDevice`; Metal only performs an MPS check (Metal has no thread-level set device API)
  - **Usage scenarios**:
    - ✅ Auto-effective when using `BatchInferencePipeline` for RL policies (PPO inference in `axon-rl`)
    - ✅ Concurrent multi-model serving (different models pinned to different cores)
    - ❌ Backtest matching engine (single-threaded deterministic design naturally does not need it)
    - ❌ LLM Agent one-shot inference (requests are low-frequency, pinning wastes resources)

### Applicable Scenarios
- Convert a trained RL policy to ONNX for low-latency inference
- Multi-model A/B (`BatchInferencePipeline` runs 2 models simultaneously)
- Model continuously updates in production (`ModelHotReloader` watches files)
- Any service that requires stable P99 latency

### Non-applicable Scenarios
- Model training (this is the inference engine, no backpropagation)
- Large-model LLM inference (use vLLM / TGI; this module is designed for small models)
- Ad-hoc one-off inference in simple scripts (load overhead exceeds benefit)

### How to Use

**Python side (primary usage; 4 inference scenarios):**

```python
from axon_quant.inference import (
    InferenceEngine, ModelConfig, Device, Observation, Action,
    InferenceBackend, BatchInferencePipeline, ModelHotReloader,
    create_onnx_engine, create_candle_engine, create_inference_engine,
    pin_current_thread_to_cpus, get_affinity_plan, InferenceError,
)
import os

# ─── 1) One-step create + load (most common) ────────────────────────────
engine = create_onnx_engine(
    model_path="model.onnx",
    input_shape=(1, 64, 128),
    output_dim=3,
    device=Device.Cpu,
    num_threads=4,
)
obs = Observation(symbol="BTC-USDT", timestamp_ns=1_000_000_000,
                  features=[0.0] * 128)
action: Action = engine.infer(obs)
print(action.action_type, action.confidence)

# ─── 2) Candle backend (pure Rust, no ONNX Runtime dependency) ───────────
engine = create_candle_engine(
    model_path="model.safetensors",
    input_shape=(1, 64, 128),
    output_dim=3,
)

# ─── 3) Batch inference pipeline (high QPS service) ───────────────────────────
pipeline = BatchInferencePipeline(
    model_config=ModelConfig(
        path="model.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.Cpu,
        input_shape=(1, 64, 128),
        output_dim=3,
    ),
    batch_window_us=2_000,          # 2ms window
    max_batch_size=64,
)
action = pipeline.submit(obs)
# P99 latency < 5ms, single-core 10k QPS

# ─── 4) Model hot reload (continuous iteration in production) ─────────────────────────
reloader = ModelHotReloader(watch_path="/var/axon/models/ppo_btc/current.onnx")
reloader.on_reload(lambda path: pipeline.update_model(path))
reloader.start()                    # watch for file changes

# ─── 5) CPU affinity pinning (production-grade low latency) ───────────────────────
# Note: auto-effective requires the following preconditions
# - ✅ BatchInferencePipeline + multi-model concurrency (recommended)
# - ❌ BacktestEngine matching (single-threaded, not needed)
# - ❌ LLM Agent one-shot inference (low-frequency requests, pinning wastes)
plan = get_affinity_plan(worker_count=4)
pin_current_thread_to_cpus([0, 1, 2, 3])   # pin current thread to cores 0-3

# Platform support: Linux/macOS ✅  Windows ❌  runtime refuses, use WSL2/numactl
```

**Rust side (use when developing new backends / embedding into RL training):**

```rust
use axon_inference::{BatchInferencePipeline, ModelConfig, InferenceBackend, Device, Observation};
use axon_inference::affinity::{pin_current_thread_to_cpus, AffinityPlan};

pin_current_thread_to_cpus(&[0, 1, 2, 3])?;

let cfg = ModelConfig {
    path: "model.onnx".into(),
    backend: InferenceBackend::Onnx,
    device: Device::Cpu,
    input_shape: [1, 64, 128],
    output_dim: 3,
    fp16: false,
    num_threads: 4,
};
let pipeline = BatchInferencePipeline::new(cfg, 2_000_000, 64)?;
let action = pipeline.submit(Observation { /* ... */ }).await?;
```

### Key Dependencies
- **Depends on**: `axon-core`, ort / tch / candle (by feature)
- **Depended on by**: `axon-rl` (policy inference), `axon-llm` (LLM backend, optional), `axon-registry` (load production model)

---

## 17. `axon-exchange`

### Core Responsibility
Exchange integration: Binance / OKX REST + WebSocket adapters + exponential-backoff reconnect + token-bucket rate limit + order lifecycle management.

### Code Location
- `crates/axon-exchange/src/lib.rs` — entry + `build_http_client`
- `crates/axon-exchange/src/traits.rs` — `ExchangeAdapter` trait
- `crates/axon-exchange/src/adapters/binance.rs` — Binance USDⓈ-M futures adapter
- `crates/axon-exchange/src/adapters/okx.rs` — OKX V5 adapter
- `crates/axon-exchange/src/ws/manager.rs` — `WebSocketManager` (auto-reconnect + circuit breaker)
- `crates/axon-exchange/src/ws/protocol.rs` — protocol codec
- `crates/axon-exchange/src/sign/binance.rs` / `sign/okx.rs` — signing
- `crates/axon-exchange/src/rate_limiter.rs` — `TokenBucketRateLimiter`
- `crates/axon-exchange/src/lifecycle.rs` — `OrderLifecycleManager` (local state machine)
- `crates/axon-exchange/src/python/` — PyO3 bindings

### Core Mechanism
- **Exponential backoff**: `ReconnectConfig { initial_backoff, max_backoff, backoff_multiplier }`, doubles on consecutive failures
- **Rate limit**: `TokenBucketRateLimiter { requests_per_second, orders_per_minute, ws_messages_per_second }` each independent
- **Signing**: `sign::binance::sign(query, secret)` → HMAC-SHA256; OKX uses HMAC-SHA256 + Base64
- **Lifecycle**: `OrderRecord { id, exchange_id, status, created_at, updated_at }`, `OrderLifecycleManager` maintains local state

### Applicable Scenarios
- Live integration with Binance / OKX futures (use `BinanceAdapter` / `OkxAdapter`)
- Testnet verification of strategies (`ExchangeConfig.testnet = true`)
- Pair with `axon-risk` for risk gating + `axon-monitor` for latency monitoring
- Local order state cache + crash recovery (`OrderLifecycleManager`)

### Non-applicable Scenarios
- US stocks / A-shares (not yet supported)
- Spot trading (current adapters are for futures)
- Cross-exchange arbitrage (needs fast switching across adapters; a single instance cannot do this)

### How to Use

**Python side (primary usage; Binance / OKX integration):**

```python
import os
from axon_quant.exchange import (
    BinanceAdapter, OkxAdapter, ExchangeId,
    binance_testnet_config, okx_testnet_config,
    OrderLifecycleManager, TokenBucketRateLimiter,
    ExchangeError,
)

# ─── 1) Binance futures: testnet by default, key from env var ─────────
os.environ["BINANCE_API_KEY"] = "your_key"
os.environ["BINANCE_API_SECRET"] = "your_secret"

cfg = binance_testnet_config()                # default testnet=True
adapter = BinanceAdapter(cfg)
adapter.connect()

# 2) Place order (synchronous; Rust side uses block_on tokio)
order_id = adapter.place_order({
    "symbol": "BTCUSDT",
    "side": "buy",
    "type": "limit",
    "quantity": "0.1",
    "price": "50000",
    "tif": "GTC",
})
print(f"Order placed: {order_id}")

# 3) Cancel / replace
adapter.cancel_order(order_id)
adapter.replace_order(order_id, new_price="50100", new_quantity="0.1")

# 4) Query order status / history
status = adapter.get_order_status(order_id)
open_orders = adapter.get_open_orders("BTCUSDT")

# 5) Local order lifecycle management (crash recovery)
mgr = OrderLifecycleManager()
cid = mgr.register_order({
    "symbol": "BTCUSDT", "side": "buy", "type": "limit",
    "quantity": "0.1", "price": "50000", "tif": "GTC",
    "exchange": "binance",
})
mgr.update_status(cid, {"status": "filled", "filled_qty": "0.1", "avg_price": "50000"})
print(mgr.active_count(), mgr.history_count())

# 6) Rate-limit protection (avoid hitting exchange API limits)
limiter = TokenBucketRateLimiter(
    requests_per_second=10,         # REST limit
    orders_per_minute=1200,         # order limit
    ws_messages_per_second=5,       # WS heartbeat limit
)
# adapter uses the limiter internally

adapter.disconnect()

# ─── OKX similar ────────────────────────────────────────
os.environ["OKX_API_KEY"] = "..."
os.environ["OKX_API_SECRET"] = "..."
os.environ["OKX_PASSPHRASE"] = "..."    # OKX-specific

okx = OkxAdapter(okx_testnet_config())
okx.connect()
okx.place_order({"symbol": "BTC-USDT-SWAP", "side": "buy", ...})
okx.disconnect()
```

**Rust side (use when developing new exchange adapters / embedding into low-level scheduling):**

```rust
use axon_exchange::{BinanceAdapter, ExchangeConfig, ExchangeId, RateLimitConfig, ReconnectConfig};

let cfg = ExchangeConfig {
    exchange_id: ExchangeId::Binance,
    api_key: std::env::var("BINANCE_KEY")?,
    api_secret: std::env::var("BINANCE_SECRET")?,
    testnet: true,
    rest_base_url: "https://testnet.binance.vision".into(),
    ws_url: "wss://testnet.binance.vision/ws".into(),
    rate_limit: RateLimitConfig::default(),
    reconnect: ReconnectConfig::default(),
};
let adapter = BinanceAdapter::new(cfg)?;
let order_id = adapter.place_limit("BTCUSDT", "BUY", 0.01, 50_000.0).await?;
```

### Key Dependencies
- **Depends on**: `axon-core` / `axon-oms` / `reqwest` / `tokio-tungstenite`
- **Depended on by**: `axon-llm::trading` (live trading agent), production deployment pipeline

---

## 18. `axon-oms`

### Core Responsibility
Order management system: state machine (New → Submitted → Acknowledged → PartiallyFilled → Filled/Cancelled/Rejected) + idempotency + snapshot/restore + batch operations.

### Code Location
- `crates/axon-oms/src/lib.rs` — entry
- `crates/axon-oms/src/manager.rs` — `OrderManager` (core)
- `crates/axon-oms/src/portfolio.rs` — `Portfolio` / `Position` / `PortfolioSnapshot`
- `crates/axon-oms/src/types.rs` — `Order` / `OrderStatus` / `Side` / `TimeInForce`
- `crates/axon-oms/src/error.rs` — `OmsError`
- `crates/axon-oms/src/python/` — PyO3 bindings

### Core Mechanism
- **Idempotency**: `Order` contains `idempotency_key`; second submit with the same key returns the original id directly
- **State machine**: `OrderStatus` transitions are guarded by `matches!` in `manager.rs`; illegal transitions return `OmsError::InvalidTransition`
- **Snapshot**: `OrderManager::snapshot() -> Vec<u8>` (4.9µs / 100 orders), can be `restore(bytes)` after crash
- **Batch**: `batch_submit` / `batch_cancel` complete under a single lock to avoid races

### Applicable Scenarios
- Live OMS (connect to `axon-exchange`)
- Simulate orderbook state during RL training (paired with `axon-backtest`)
- Persist orders (snapshot → save to disk → restore on startup)
- Idempotent protection: never double-submit on network retry

### Non-applicable Scenarios
- Historical replay (that is the domain of `axon-backtest`)
- Matching itself (`axon-oms` only manages order lifecycle, not matching)
- Cross-account management (OMS is single-account; multi-account should be wrapped at a higher layer)

### How to Use

**Python side (primary usage; complete lifecycle management):**

```python
from axon_quant.oms import (
    OrderManager, Order, OrderStatus, Side, OrderType,
    Portfolio, Position, OmsError,
    limit_order, market_order, make_order_status,
)
from decimal import Decimal

# 1) Start OMS + initial funds
oms = OrderManager()
oms.deposit("USDT", 100_000)

# 2) Submit an order (factory functions handle Decimal precision automatically)
oid = oms.submit(limit_order(
    symbol="BTC-USDT", side="Buy",
    quantity=0.1, price=50_000,
    idempotency_key="ppo-btc-20260715-001",  # idempotency key
))
print(f"Order id: {oid}")

# 3) Advance the state machine
oms.update_status(oid, make_order_status("Acknowledged"))
oms.update_status(oid, make_order_status("Submitted"))

# 4) Handle partial / full fill
oms.add_fill(
    order_id=oid,
    fill_id="f-001",
    symbol="BTC-USDT",
    price=50_000,
    quantity=0.05,          # positive=buy, negative=sell
    fee=5.0,
)
oms.update_status(oid, make_order_status("PartiallyFilled",
                                         filled_qty=0.05, avg_price=50_000))

oms.add_fill(order_id=oid, fill_id="f-002",
             symbol="BTC-USDT", price=50_010, quantity=0.05, fee=5.0)
oms.update_status(oid, make_order_status("Filled",
                                         filled_qty=0.1, avg_price=50_005))

# 5) Cancel / reject
oms.cancel(oid)
# oms.update_status(oid, make_order_status("Rejected", reason="insufficient"))

# 6) Idempotency: same-key second submit returns the original id, no duplicate
oid2 = oms.submit(limit_order(
    symbol="BTC-USDT", side="Buy",
    quantity=0.1, price=50_000,
    idempotency_key="ppo-btc-20260715-001",  # same key
))
assert oid == oid2

# 7) Snapshot (4.9µs / 100 orders) + crash recovery
snap_bytes = oms.snapshot()                     # Vec<u8>
with open("/var/axon/oms/snap.bin", "wb") as f:
    f.write(snap_bytes)
# After restart:
# oms.restore(snap_bytes)

# 8) Query portfolio
bal = oms.snapshot_balance()
print(bal["cash"]["USDT"])                       # remaining cash
for pos in bal["positions"]:
    print(pos["symbol"], pos["quantity"], pos["avg_price"], pos["realized_pnl"])

print(f"active={oms.active_count()} history={oms.history_count()}")
```

**Rust side (use when developing new Portfolio algorithms / embedding into low-level scheduling):**

```rust
use axon_oms::{OrderManager, Order, OrderStatus, Side, OrderType};
use rust_decimal::Decimal;

let oms = OrderManager::new();
let order = Order::new("BTC-USDT".into(), Side::Buy, OrderType::Limit,
    Decimal::new(1, 3), Decimal::from(50_000));
let id = oms.submit(order)?;
oms.update_status(id, OrderStatus::Acknowledged)?;
```

### Key Dependencies
- **Depends on**: `axon-core` / `rust_decimal`
- **Depended on by**: `axon-exchange` (live submit), `axon-llm::trading` (LLM order tools), `axon-risk` (pre-check)

---

## 19. `axon-monitor`

### Core Responsibility
Production monitoring: atomic metrics (Counter / Gauge / Histogram) + alert rules + health checks + Prometheus export.

### Code Location
- `crates/axon-monitor/src/lib.rs` — entry
- `crates/axon-monitor/src/metrics.rs` — `AtomicCounter` / `AtomicGauge` / `LatencyHistogram` / `LatencyPercentiles`
- `crates/axon-monitor/src/registry.rs` — `MetricsRegistry`
- `crates/axon-monitor/src/alert.rs` — `AlertRule` / `AlertEvent` / `ThresholdCondition`
- `crates/axon-monitor/src/health.rs` — `HealthService` / `ComponentHealth`
- `crates/axon-monitor/src/error.rs` — `MonitorError`

### Core Mechanism
- **Atomic metrics**: `AtomicCounter` uses `AtomicU64::fetch_add` (1.6ns), `AtomicGauge` uses `AtomicU64` to store bits (464ps)
- **Histogram**: `LatencyHistogram` uses fixed buckets (ns/µs/ms/s) + atomic counters
- **Alerts**: register `AlertRule::Threshold { metric, condition, severity, message }`; `check_alerts(name, value)` triggers it
- **Health check**: `HealthService` collects each component's `HealthCheck::check() -> ComponentHealth`

### Applicable Scenarios
- Expose Prometheus metrics from the live service (`/metrics` endpoint)
- Alert when order latency P99 exceeds a threshold (hook into Slack / PagerDuty)
- Kubernetes liveness / readiness probes (use `HealthService`)
- Performance baseline (every nanosecond is recorded; can plot flame graphs)

### Non-applicable Scenarios
- Business semantic metrics (this is a low-level counter, not BI)
- Long-term storage (push to Prometheus + remote TSDB)
- Complex alert routing (use Alertmanager)

### How to Use

> **Important: `axon-monitor` is intentionally NOT exposed to Python (this is a design constraint, not an oversight).**
> Python users who need monitoring capabilities use two equivalent paths, see "Python-side Equivalent Capabilities" below.

#### Why It Is Not Exposed to Python

| Dimension | `axon-monitor` design | What exposing to Python would break |
|----------|----------------------|-----------------------------------|
| **Performance** | Counter inc **1.6ns** / Gauge set **464ps** / Histogram observe **6.5ns** / Alert check **4.8ns** | A PyO3 cross-language call itself costs **~100ns+**, turning a 1.6ns `inc` into 100ns+ (60x regression) and breaking lock-free atomic guarantees |
| **Usage scope** | In-process instrumentation primitives for matching / order / hot paths; nanosecond-level observability | Python should not enter the hot path (would pollute P99 trading latency) |
| **Output destination** | Rust-side write → `axum` HTTP `/metrics` endpoint → Prometheus scrape | Python uses `prometheus_client` to scrape directly; no need to go through PyO3 |
| **Python equivalent** | — | `axon-tracker` already covers training/experiment monitoring (`MLflow` / `WandB` / `Local` / `Memory` backends) |

Code-level confirmation (as of `0.4.1`):

- `crates/axon-monitor/Cargo.toml` has **no `python` feature**
- `crates/axon-monitor/src/` has **no `python/` sub-module** (`metrics.rs` / `registry.rs` / `alert.rs` / `health.rs` have no `#[pyclass]`)
- `crates/axon-python/Cargo.toml` python-feature list **does not include `dep:axon-monitor`**
- `crates/axon-python/src/lib.rs` `_native` function has **no `monitor` sub-module** registration (in contrast to `backtest` / `risk` / `oms` which are all registered)

#### Python-side Equivalent Capabilities (Already Available)

```python
# Option A: training / experiment metrics — use axon-tracker (already exposed)
from axon_quant.tracker import MemoryTracker, WandBTracker, MLflowTracker

tracker = MemoryTracker(experiment_name="ppo_btc_v1")
tracker.log_metric("sharpe", 1.23, step=1000)
tracker.log_metric("max_drawdown", -0.08, step=1000)
tracker.log_param("lr", 3e-4)
tracker.finish()

# Option B: production service metrics — scrape the Rust-side /metrics endpoint
import requests
from prometheus_client.parser import text_string_to_metric_families

resp = requests.get("http://trading-host:9090/metrics")
for family in text_string_to_metric_families(resp.text):
    for sample in family.samples:
        print(sample.name, sample.labels, sample.value)
        # e.g. orders_total {side="buy", symbol="BTCUSDT"} 1234
```

#### Rust Side (Use When Developing New Metric Types / Embedding Instrumentation in oms/exchange)

```rust
use axon_monitor::{
    MetricsRegistry, AlertRule, AlertSeverity, ThresholdCondition,
    HealthService, HealthStatus, ComponentHealth,
};
use axum::{routing::get, Router};

// 1) Build registry at startup
// Note: `register_*` actually only takes a name, no labels / custom-bucket parameter
//     (KISS design — labels are injected by the Prometheus exporter stage)
let mut reg = MetricsRegistry::new();
let order_count = reg.register_counter("orders_total");
let order_latency = reg.register_histogram("order_latency_ns");  // default buckets
let nav_gauge = reg.register_gauge("portfolio_nav");

// 2) Instrument in business hot path (nanosecond-level, lock-free)
order_count.inc();                       // +1
order_count.inc_by(3);                  // +3
order_latency.observe(150_000.0);       // 150µs, unit is ns by convention
nav_gauge.set(102_345.67);

// 3) Alert rules (check_alerts triggers and records, get_alerts retrieves)
reg.add_alert_rule(AlertRule::Threshold {
    metric_name: "order_latency_ns".into(),
    condition: ThresholdCondition::GreaterThan(10_000_000.0),  // 10ms
    severity: AlertSeverity::Warning,
    message: "order latency P99 > 10ms".into(),
});
// In the order path: reg.check_alerts("order_latency_ns", observed_value);
// At the health endpoint: let alerts = reg.get_alerts();

// 4) Health check (K8s liveness / readiness probe)
// Note: the actual HealthService API is `check(Vec<ComponentHealth>) -> HealthCheck`,
//     the caller collects each component's current state and aggregates in one shot
//     (no register/callback mechanism), so it doesn't bind to an async runtime
//     (works with tokio / async-std / no async), keeping it 0-dependency.
let health = HealthService::new();
let report = health.check(vec![
    ComponentHealth { name: "oms".into(),      status: HealthStatus::Healthy, message: "ok".into() },
    ComponentHealth { name: "exchange".into(), status: HealthStatus::Healthy, message: "ok".into() },
]);
// report.status / report.components / report.uptime_secs can be JSON-serialized directly

// 5) Expose axum routes (Prometheus scrape + K8s probe)
// Note: axon-monitor does not bind to an HTTP framework (to avoid version-locking with axum / actix).
//     Prometheus text-format output and health JSON responses are implemented by the caller.
let app = Router::new()
    .route("/metrics", get(/* prometheus exporter impl */ async { "" }))
    .route("/health",  get(/* health JSON impl */        async { "" }))
    .route("/ready",   get(/* readiness impl */           async { "" }));
```

### Key Dependencies
- **Depends on**: `axon-core`
- **Depended on by**: `axon-exchange` (latency monitoring), `axon-oms` (order counter), production services

---

## 20. `axon-defi`

### Core Responsibility
DeFi on-chain trading: EVM RPC + signing + ERC-20 + Uniswap V3 routing/quoter/pool + LayerZero cross-chain + MEV-Share.

### Code Location
- `crates/axon-defi/src/lib.rs` — entry (`VERSION`)
- `crates/axon-defi/src/evm/provider.rs` — `EvmProvider` (RPC client)
- `crates/axon-defi/src/evm/chain.rs` — `Chain` / `ChainSpec`
- `crates/axon-defi/src/evm/erc20.rs` — `Erc20` (contract binding)
- `crates/axon-defi/src/evm/signer.rs` — private key signing
- `crates/axon-defi/src/evm/multicall.rs` — Multicall3 batch call
- `crates/axon-defi/src/dex/uniswap.rs` — Uniswap V2 router
- `crates/axon-defi/src/dex/v3_router.rs` / `v3_quoter.rs` / `v3_pool.rs` — Uniswap V3
- `crates/axon-defi/src/bridge/layerzero.rs` — LayerZero cross-chain
- `crates/axon-defi/src/mev/share.rs` — MEV-Share integration
- `crates/axon-defi/src/python/` — PyO3 bindings (**requires `evm` feature enabled**)

### Core Mechanism
- **EVM provider**: based on `ethers-rs` (feature-gated)
- **Multicall**: uses the Multicall3 contract to get the results of multiple read-only calls in a single RPC
- **V3 quoter**: `v3_quoter::quote_exact_input_single` estimates the swap output
- **MEV-Share**: submits the transaction bundle to Flashbots, protecting against sandwich

### Applicable Scenarios
- On-chain market making / arbitrage (Uniswap V2/V3)
- Large swaps that need to avoid MEV loss (use `mev::share`)
- Cross-chain bridging (LayerZero)
- Wallet integration (`signer` + `erc20::transfer`)

### Non-applicable Scenarios
- CEX arbitrage (that is `axon-exchange`)
- Real-time high-frequency on-chain trading (12s block time is not suited for HFT)
- Non-EVM chains (Solana / Sui not yet supported)

### How to Use

**Python side (primary usage; 5 on-chain trading scenarios):**

```python
import asyncio
from axon_quant.defi import (
    Chain, EvmConfig, DefiOrder, evm_provider, local_signer, erc20_client,
    V3Quoter, Multicall, BridgeManager, MevShareClient,
    DefiError,
)
# Note: the defi module requires the `evm` feature to be enabled

# ─── 1) Provider + query on-chain state ─────────────────────────
provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
print(await provider.chain_id())       # 1
print(await provider.block_number())   # current block height

# ─── 2) ERC-20 balance query (via Multicall batch) ──────────────────
usdc = erc20_client("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", provider)
print(await usdc.symbol())             # "USDC"
print(await usdc.decimals())           # 6
print(await usdc.balance_of("0xYourAddress"))

# Multicall: query N addresses in a single RPC
mc = Multicall(provider)
balances = await mc.balance_of_batch(usdc, [
    "0xAddr1", "0xAddr2", "0xAddr3",
])
# balances: ['1000000000', '500000000', '0']

# ─── 3) Uniswap V3 quote (read-only, no transaction) ─────────────────────
quoter = V3Quoter(provider)
amount_out = await quoter.quote_exact_input_single(
    token_in="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",   # WETH
    token_out="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
    fee_tier=3000,                                          # 0.3%
    amount_in="1000000000000000000",                        # 1 WETH
)
print(f"1 WETH ≈ {amount_out} USDC")

# ─── 4) Real swap (write to chain, needs signer + V3 Router) ─────────────────
signer = local_signer(private_key="0xYourPrivateKey")
order = DefiOrder.swap_exact_in(
    token_in="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
    token_out="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
    fee_tier=3000,
    amount_in="1000000000000000000",
    min_amount_out=amount_out * 0.99,        # 1% slippage protection
    recipient=signer.address,
    deadline=int(time.time()) + 300,
)
tx_hash = await signer.send_v3_swap(provider, order)
print(f"TX sent: https://etherscan.io/tx/{tx_hash}")

# ─── 5) Large swap MEV protection: via Flashbots MEV-Share ─────────
mev = MevShareClient(auth_signer=signer, endpoint="https://relay.flashbots.net")
tx_hash = await mev.submit_transaction(signed_tx_bytes, ...)
# bundle mode automatically protects against sandwich

# ─── 6) Cross-chain bridge (LayerZero V2) ───────────────────────────────
bridge = BridgeManager()
print(bridge.is_supported(src_chain=Chain.Ethereum, dst_chain=Chain.Arbitrum))
```

**Rust side (use when developing new EVM adapters):**

```rust
#[cfg(feature = "evm")]
use axon_defi::{EvmProvider, Chain, Erc20};

let provider = EvmProvider::connect(Chain::Ethereum, "https://eth.llamarpc.com").await?;
let usdc = Erc20::new(USDC_ADDR, provider.clone());
let balance = usdc.balance_of(my_addr).await?;
```

### Key Dependencies
- **Depends on**: `axon-core`, ethers / alloy (feature-gated)
- **Depended on by**: DeFi strategy research, cross-chain bridge integration

---

## 21. `axon-harness`

### Core Responsibility
**Trait interfaces + safety components** for the Harness orchestration system: circuit breaker, audit chain, position guard + default adjudication policy + RBAC tool gating + token budget guard + observability.

### Code Location
- `crates/axon-harness/src/lib.rs` — entry
- `crates/axon-harness/src/policy.rs` — `HarnessPolicy` / `ToolGate` / `BudgetGuard` trait
- `crates/axon-harness/src/types.rs` — `AgentIntent` / `TaskContext` / `HarnessResult`
- `crates/axon-harness/src/default_policy.rs` — `DefaultPolicy` (combine ToolGate + BudgetGuard + Risk)
- `crates/axon-harness/src/simple_budget.rs` — `SimpleBudgetGuard` (token usage cap)
- `crates/axon-harness/src/rbac_gate.rs` — `RBACToolGate` (role-based tool access)
- `crates/axon-harness/src/bridge.rs` — `HarnessBridge` (connect LLM Agent to Harness)
- `crates/axon-harness/src/observer.rs` — `HarnessObserver` (decision recording / metrics)
- `crates/axon-harness/src/circuit_breaker.rs` — `CircuitBreaker` (AtomicU8 state machine, < 20ns)
- `crates/axon-harness/src/audit.rs` — `AuditChain` (Blake3 hash chain)
- `crates/axon-harness/src/position.rs` — `PositionGuard`

### Core Mechanism
- **Three-stage gate**: `HarnessBridge` runs before every agent tool call:
  1. `RBACToolGate` (does the role have permission?)
  2. `SimpleBudgetGuard` (any token budget left?)
  3. `PositionGuard` + `CircuitBreaker` (position / circuit breaker allowed?)
- **Blake3 audit chain**: `AuditChain::append(entry) -> entry`, hash = `blake3(prev_hash || payload)`
- **Observability**: `HarnessObserver::record_decision` writes metrics + decisions to `axon-tracker`

### Applicable Scenarios
- Unified safety layer for LLM Agent tool calls (`axon-llm` uses this by default)
- Any agent orchestration that needs role / budget / risk gating
- In multi-agent collaboration, record each agent's decision into `AuditChain`
- Linked with `axon-risk` (Risk rejection also goes into `AuditChain`)

### Non-applicable Scenarios
- Internal scripts that do not need safety gating
- Single-machine non-agent systems (use `axon-oms` directly)
- Cross-process transactions (the audit chain is single-process; multi-process needs external storage)

### How to Use

**Python side (primary usage; 3 categories):**

```python
from axon_quant.harness import (
    HarnessBridge, HarnessPolicy, DefaultPolicy,
    RBACToolGate, SimpleBudgetGuard, PositionGuard,
    CircuitBreaker, AuditChain, HarnessObserver,
    PlaceOrderTool, QueryPortfolioTool, CancelOrderTool, ReplaceOrderTool,
    MockTradingBackend, RiskLimits,
)
from axon_quant.harness.tools import ToolRole  # role enum

# 1) Build the default policy (RBAC + budget + risk, three-stage gate)
policy = DefaultPolicy(
    tool_gate=RBACToolGate.strict(allowed_roles={ToolRole.Trader}),
    budget_guard=SimpleBudgetGuard(max_tokens=100_000),   # LLM budget
    position_guard=PositionGuard(max_position_per_symbol=100.0,
                                 max_leverage=3.0),
    circuit_breaker=CircuitBreaker(max_consecutive_losses=5,
                                   cooldown_seconds=300),
)
bridge = HarnessBridge(policy=policy, observer=HarnessObserver())

# 2) Register tools (every tool call goes through the three-stage gate)
backend = MockTradingBackend()    # in production, connect to axon-exchange
risk_limits = RiskLimits(
    max_order_notional=10_000.0,
    max_daily_orders=100,
    allowed_symbols=["BTC-USDT", "ETH-USDT"],
)
place_tool = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk_limits)
query_tool = QueryPortfolioTool(backend=backend)
cancel_tool = CancelOrderTool(backend=backend, risk=risk_limits)
replace_tool = ReplaceOrderTool(backend=backend, risk=risk_limits)

bridge.register_tool("place_order", place_tool, role=ToolRole.Trader)
bridge.register_tool("query_portfolio", query_tool, role=ToolRole.Trader)
bridge.register_tool("cancel_order", cancel_tool, role=ToolRole.Trader)

# 3) Hook into an LLM Agent (ReAct / Swarm) for automatic gating
# Before any LLM tool call, HarnessBridge automatically:
#  a) Role check (does the LLM's current role have permission?)
#  b) Budget check (token budget remaining?)
#  c) Risk check (position / circuit breaker allowed?)
#  d) Audit trail (Blake3 hash chain append)

# 4) Manual invocation (same gating + audit)
result = bridge.invoke(
    tool_name="place_order",
    caller_role=ToolRole.Trader,
    args={"symbol": "BTC-USDT", "side": "Buy", "quantity": 0.1, "price": 50000.0},
)
print(result.success, result.output)

# 5) Audit chain verification (run after crash recovery)
audit: AuditChain = bridge.audit_chain()
assert audit.verify_integrity()    # hash chain not tampered
print(f"Audit events: {audit.event_count}")

# 6) Observability (decision records → push to axon-tracker)
observer: HarnessObserver = bridge.observer()
observer.export_to_tracker(axon_quant.tracker.MemoryTracker())
```

**Rust side (use when developing new Gates / new Policies):**

```rust
use axon_harness::{HarnessBridge, DefaultPolicy, RBACToolGate, SimpleBudgetGuard};

let policy = DefaultPolicy::new()
    .with_tool_gate(RBACToolGate::strict("trader"))
    .with_budget(SimpleBudgetGuard::new(100_000));
let bridge = HarnessBridge::new(policy);
let result = bridge.invoke("place_order", args)?;
```

### Key Dependencies
- **Depends on**: `axon-core` / `blake3`
- **Depended on by**: `axon-llm` (agent tool-call gating), `axon-oms` (actual order placement)

---

## 22. `axon-integration-tests`

### Core Responsibility
Cross-crate end-to-end tests + property tests + contract tests. Compiled only for tests; not part of release.

### Code Location
- `crates/axon-integration-tests/src/lib.rs` — entry
- `crates/axon-integration-tests/src/matching_flow.rs` — scenario 1: backtest matching
- `crates/axon-integration-tests/src/hpo_flow.rs` — scenario 3: HPO full flow
- `crates/axon-integration-tests/src/walkforward_flow.rs` — scenario 4: Walk-Forward
- `crates/axon-integration-tests/src/distributed_flow.rs` — scenario 6: distributed training
- `crates/axon-integration-tests/src/tracker_registry_flow.rs` — scenario 5: tracking + registry
- `crates/axon-integration-tests/src/e2e_pipeline.rs` — end-to-end 4-crate chain
- `crates/axon-integration-tests/src/phase4_e2e.rs` — Phase 4 (production deployment chain)
- `crates/axon-integration-tests/src/contract.rs` — API / data contract stability
- `crates/axon-integration-tests/src/fuzz.rs` — proptest property tests
- `crates/axon-integration-tests/src/fixtures.rs` — shared fixtures

### Core Mechanism
- **Scenario organization**: each `_flow.rs` corresponds to one business scenario, chaining multiple crates
- **Property-based**: uses `proptest` to auto-generate inputs and verify invariants
- **Contract tests**: snapshot serialized results; check compatibility across versions

### Applicable Scenarios
- Run `cargo test -p axon-integration-tests` to verify the full chain
- When adding a new module, write the corresponding `_flow.rs` scenario
- CI blocking (see `.github/workflows/validation.yml`)

### Non-applicable Scenarios
- Single-crate unit tests (use each crate's own `tests/` directory)
- Performance benchmarks (use `benches/`)
- Real exchange e2e (that is the `e2e-real-llm.yml` workflow)

### How to Use

> `axon-integration-tests` is a **Rust-side cross-crate integration test framework**; it is not exposed in the Python wheel.
> Python users typically do not need to use this module directly. To run the full regression, execute the following in the repo root:

```bash
# Run all integration tests (local development)
cargo test -p axon-integration-tests --features all

# Run a specific scenario (e.g. e2e_pipeline end-to-end 4-crate chain)
cargo test -p axon-integration-tests --test e2e_pipeline

# Run proptest property tests
cargo test -p axon-integration-tests fuzz

# CI blocking
.github/workflows/validation.yml   # auto-runs all scenarios
```

**Rust side (use when developing new scenarios / writing `_flow` for a new module):**

```rust
use axon_integration_tests::fixtures;

#[test]
fn my_new_flow() {
    let (oms, risk, mock_exchange) = fixtures::full_stack();
    // 1. Prepare: backtest data + risk config
    // 2. Run scenario: backtest -> HPO -> evaluate -> register
    // 3. Assert: full-chain Sharpe > baseline
}
```

### Key Dependencies
- **Depends on**: nearly all `axon-*` crates
- **Depended on by**: CI workflows

---

## 23. `axon-python`

### Core Responsibility
Python unified entry `axon_quant._native`: aggregates each crate's PyO3 bindings + shared exception base class into one module.

### Code Location
- `crates/axon-python/src/lib.rs` — `#[pymodule] _native` entry
- `crates/axon-python/src/error.rs` — common exception base class `AxonError` + 6 subclasses
- `crates/axon-python/src/harness.rs` — Harness Python bindings

### Core Mechanism
- **Unified exception**: `register_exceptions` registers base classes **before** the submodule's `create_exception!`
- **Feature gating**: compiles only when the `python` feature is enabled (`#![cfg(feature = "python")]`)
- **Avoid cycles**: does not depend on each crate's `python` submodule (they register independently to the submodule name)

### Applicable Scenarios
- Python users get the full capability after `import axon_quant`
- After installing the wheel with `pip install axon-quant`, can `import axon_quant.rl` etc.
- Runs on PyO3 0.28 + Python 3.12+

### Non-applicable Scenarios
- Pure Rust projects (use each crate directly; no need for this aggregation)
- Embedded environments (the Python interpreter is too heavy)

### How to Use

```python
import axon_quant
print(axon_quant.__version__)  # 0.4.0

# Submodules
env = axon_quant.rl.TradingEnv(...)
df = axon_quant.data.CsvSource(...)
risk = axon_quant.risk.DefaultRiskEngine(...)
```

### Key Dependencies
- **Depends on**: all crates with the `python` feature
- **Depended on by**: Python user code, PyPI release

---

## Module Dependency Quick Reference

```text
axon-core ◄── axon-backtest ◄── axon-rl
        ▲                ▲           │
        │                │           ├── axon-hpo
        │                │           ├── axon-walk-forward
        │                │           ├── axon-distributed
        │                │           └── axon-tracker
        │                │           │
        │                │           └── axon-registry
        │                │
        │                ├── axon-data
        │                ├── axon-llm ───► axon-explain
        │                │       │
        │                │       └──► axon-oms ◄── axon-risk
        │                │                  ▲
        │                │                  │
        │                └──► axon-exchange ┘
        │
        ├── axon-compliance
        ├── axon-monitor
        ├── axon-inference
        ├── axon-defi
        ├── axon-harness ──► axon-llm / axon-oms
        └── axon-integration-tests (test-only, depends on all)
              ▲
              │
        axon-python (aggregates the above crates with python feature)
```

---

## Module Selection Decision Tree

| You want to… | Use this module |
|-------------|-----------------|
| Implement reproducible backtests | `axon-backtest::BacktestEngine` |
| Train RL policies | `axon-rl::TradingEnv` + `axon-tracker` |
| Run hyperparameter search | `axon-hpo` + `axon-tracker` |
| Walk-forward validation | `axon-walk-forward` |
| Multi-node multi-GPU training | `axon-distributed` |
| Track experiment metrics | `axon-tracker` |
| Manage model versions | `axon-registry` |
| LLM agent trading | `axon-llm` + `axon-harness` |
| Explain model decisions | `axon-explain` |
| Multi-strategy fusion | `axon-ensemble` |
| Read historical data | `axon-data` |
| Compliance audit | `axon-compliance` |
| Pre-trade risk control | `axon-risk` |
| Inference service | `axon-inference` |
| Live order placement | `axon-exchange` + `axon-oms` + `axon-risk` |
| Order lifecycle | `axon-oms` |
| Production monitoring & alerting | `axon-monitor` |
| On-chain trading | `axon-defi` |
| Agent safety gating | `axon-harness` |
| Python entry | `axon-python` |
| Cross-module end-to-end tests | `axon-integration-tests` |
