<div align="center">

# <img src="docs/assets/logo.svg" width="36" alt="" style="vertical-align: middle;"/> AXON

**AI-Native Quantitative Trading Framework**

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](./LICENSE) [![Rust](https://img.shields.io/badge/Rust-1.96%2B-orange.svg)](https://www.rust-lang.org/) [![Python](https://img.shields.io/badge/Python-3.14%2B-3776AB.svg)](https://www.python.org/) [![Version](https://img.shields.io/badge/Version-0.3.0-green.svg)](./CHANGELOG.md) [![CI](https://img.shields.io/github/actions/workflow/status/pengwow/axon_quant/validation.yml?label=CI)](https://github.com/pengwow/axon_quant/actions) [![Tests](https://img.shields.io/badge/Tests-2300%2B-brightgreen.svg)](./crates/)

English | **[中文](./README_CN.md)**


</div>

> An event-driven trading engine for quantitative trading and reinforcement learning. Designed from the ground up with AI at its core, rather than "bolting on" ML modules to a traditional quant system.

Rust core for high-performance, Python interface for RL training, one codebase for the complete pipeline from backtesting to production.

[Online Documentation](https://pengwow.github.io/axon_quant/en/) · [Examples](./examples/)

---

## Design Philosophy

- **AI First**: RL environment and backtesting engine share the same data structures, zero difference between training and production
- **Rust Core**: Nanosecond timestamps, deterministic matching, zero-cost abstractions, backtesting throughput > 1M events/sec
- **Python Front**: Gymnasium-compatible interface via PyO3, directly compatible with Stable-Baselines3 / Ray RLlib
- **Full Pipeline**: Backtest → Train → HPO → Walk-forward → Track → Register → Deploy, all built-in
- **100% Open Source**: Apache-2.0 license, no enterprise edition, no feature restrictions

---

## Features

### Backtesting Engine

- **Multi-Level Matching**: L1 basic matching → L2 order book → L3 multi-asset crossing
- **Impact Models**: Almgren-Chriss permanent/temporary impact + probabilistic latency + tiered fees
- **Deterministic Replay**: `SimulatedClock` + crossbeam-channel bounded 100K event queue
- **Columnar Storage**: Arrow/Parquet, 1M tick read/write < 15ms

### RL Environment

- **Gymnasium API**: Discrete / continuous / hybrid action spaces
- **Reward Functions**: PnL / Sharpe / Sortino, based on unified `ReturnHistory`
- **Vectorized**: `VecEnv` supports multi-environment parallel rollout
- **PyO3 Bindings**: maturin packaging, 6 submodules

### Training Pipeline

- **Hyperparameter Optimization**: Optuna integration + NSGA-II multi-objective + Pareto frontier + early stopping
- **Rolling Forward Validation**: Purged + Embargo + leakage detection + Deflated Sharpe Ratio
- **Experiment Tracking**: MLflow / WandB / Local / Memory four backends
- **Model Registry**: SemVer + stage lifecycle + auto-archiving + rollback
- **Distributed Training**: Ray Actor + Parameter Server + Checkpoint fault tolerance

### AI Enhancement

- **LLM Agents**: ReAct + Tool Calling, built-in `PlaceOrder` / `QueryPortfolio` trading tools with SafetyMode risk control
- **Agent Swarm**: Multi-Agent collaboration with Actor model, voting consensus, dynamic scaling
  - **MarketAgent**: Market analysis and signal generation
  - **RiskAgent**: Pre-trade risk assessment and compliance checks
  - **ExecutionAgent**: Order execution with TWAP/VWAP strategies
  - **AuditAgent**: Decision logging and compliance reporting
  - **SwarmOrchestrator**: Agent lifecycle management, message routing, auto-scaling
- **Model Ensemble**: Voting / Stacking / dynamic weighting, online Sharpe ratio monitoring for auto-adjustment
- **Explainability**: SHAP feature attribution + counterfactual explanations + `Explainer` trait built-in
- **Compliance Audit**: Immutable trade logs + decision report archiving

### Production Deployment

- **Exchange Integration**: Binance / OKX REST + WebSocket (auto-reconnect)
- **Risk Engine**: Pre-trade checks (12ns), real-time circuit breaker, position limits
- **Inference Engine**: ONNX / Candle dual backends + CPU/GPU affinity pinning + batch inference

### DeFi Integration (Experimental)

> **Note**: DeFi features are experimental and under active development. APIs may change.

- **EVM Chain Support**: Ethereum / Arbitrum / Optimism / Polygon
- **DEX Integration**: Uniswap V3 direct integration with optimal routing
- **MEV Protection**: MEV-Share for sandwich attack prevention
- **Smart Contract Risk**: Hybrid risk checks (off-chain fast + on-chain authoritative)
- **Cross-Chain Bridge**: LayerZero integration for multi-chain asset transfers

---

## Quick Start

### Install (Recommended)

```bash
# Basic install (core + data processing)
pip install axon_quant

# With ONNX inference support (onnxruntime, auto-loaded)
pip install axon_quant[onnx]

# With RL training dependencies (gymnasium, stable-baselines3, torch)
pip install axon_quant[rl]

# Full install
pip install axon_quant[onnx,rl]
```

Verify installation:

```bash
python -c "import axon_quant; print(axon_quant.__version__)"
```

### Build from Source

For developers who want to modify the Rust core:

```bash
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# Build
cargo build

# Test (2300+ test cases)
cargo test --workspace

# Static analysis
cargo clippy --workspace -- -D warnings

# Build and install Python wheel
maturin build --release
pip install target/wheels/axon_quant-*.whl
```

### Training Examples

```bash
# Random baseline
python examples/01_random_agent.py

# PPO training
python examples/02_train_ppo.py --timesteps 50000

# HPO optimization
python examples/03_hpo/hpo_single_objective.py

# Rolling forward validation
python examples/08_walk_forward/walk_forward_basic.py
```

> 📖 Detailed RL training documentation: [RL Training Guide](docs/en/user-guide/rl-training.md)

---

## Architecture

AXON uses Cargo Workspace to manage 21 crates, organized in 9 layers:

```
┌─────────────────────────────────────────────────────────────┐
│  Layer 9: Application Entry                                  │
│  ├─ axon-cli        CLI tool                                 │
│  └─ axon-python     PyO3 unified entry (axon_quant package)  │
├─────────────────────────────────────────────────────────────┤
│  Layer 8: AI Agents                                          │
│  ├─ axon-llm        ReAct agent + Tool Calling               │
│  └─ axon-explain    SHAP / Counterfactual / Decision Report  │
├─────────────────────────────────────────────────────────────┤
│  Layer 7: Model Services                                     │
│  ├─ axon-inference  ONNX / Candle / tch inference engine     │
│  └─ axon-ensemble   Model ensemble (Voting / Stacking)       │
├─────────────────────────────────────────────────────────────┤
│  Layer 6: Training Pipeline                                  │
│  ├─ axon-rl         Gymnasium env + VecEnv + Reward functions│
│  ├─ axon-hpo        Optuna hyperparameter optimization       │
│  ├─ axon-distributed Ray Actor distributed training          │
│  └─ axon-walk-forward Rolling forward validation            │
├─────────────────────────────────────────────────────────────┤
│  Layer 5: Experiment Governance                               │
│  ├─ axon-tracker    MLflow / WandB / Local / Memory tracking │
│  └─ axon-registry   Model registry (SemVer + Lifecycle)      │
├─────────────────────────────────────────────────────────────┤
│  Layer 4: Production Execution                                │
│  ├─ axon-exchange   Binance / OKX adapters (REST + WebSocket)│
│  ├─ axon-risk       Risk engine (Position / Drawdown / VaR)  │
│  ├─ axon-oms        Order management system                  │
│  └─ axon-monitor    Monitoring + Health checks               │
├─────────────────────────────────────────────────────────────┤
│  Layer 3: Backtesting Engine                                  │
│  ├─ axon-backtest   L1/L2/L3 matching + Almgren-Chriss impact│
│  └─ axon-compliance Compliance audit + Reports               │
├─────────────────────────────────────────────────────────────┤
│  Layer 2: Data Services                                       │
│  └─ axon-data       Arrow columnar storage + CSV/Parquet     │
├─────────────────────────────────────────────────────────────┤
│  Layer 1: Core Types                                          │
│  └─ axon-core       Timestamp / Price / Quantity / Order     │
│                     / Event / Queue / Portfolio / SIMD        │
└─────────────────────────────────────────────────────────────┘
```

### Threading Model

- **Core Matching Engine**: Single-threaded, avoids lock contention, ensures determinism
- **I/O Thread Pool**: tokio runtime, handles WebSocket / REST / file I/O
- **Compute Thread Pool**: rayon, factor calculation / data transformation / parallel backtesting
- **Event Queue**: crossbeam-channel bounded 100K, zero-lock design

### Data Pipeline

All AXON modules share the same Arrow `RecordBatch`, zero-copy passthrough, no format conversion gaps:

```
Data Sources (CSV/Parquet/WebSocket/Mock/Exchange API)
    │
    ▼
axon-data (schema validation / time alignment / dedup / mmap cache)
    │
    ▼
Arrow RecordBatch (memory) ──→ TradingEnv / FeaturePipeline / BacktestEngine
    │
    ▼
InferenceEngine (ONNX/Candle batch inference < 1ms)
    │
    ▼
ExchangeAdapter (Binance/OKX live trading)
```

### Layer Descriptions

1. **axon-core**: Foundation of the entire system. Provides `Timestamp` (nanosecond precision), `Price` / `Quantity` (based on `rust_decimal`), `Order`, `Event`, `Queue`, `Portfolio` core types, and SIMD-accelerated normalization and order book operations.

2. **axon-data**: Unified data access layer. Based on Apache Arrow's `RecordBatch` columnar storage, supports CSV / Parquet / Mock data sources, built-in `FeaturePipeline` (Z-Score normalization + sliding window).

3. **axon-backtest**: Event-driven backtesting engine. Supports L1 (price priority), L2 (order book), L3 (dark pool / auction) three-level matching, integrated Almgren-Chriss market impact model and probabilistic latency simulation.

4. **axon-exchange**: Production-grade exchange adapter. Unified `ExchangeAdapter` trait, implemented Binance / OKX REST + WebSocket integration, built-in exponential backoff reconnection and token bucket rate limiting.

5. **axon-rl**: Reinforcement learning environment. `TradingEnv` implements Gymnasium standard interface (`reset` / `step` / `render`), supports continuous actions (target position ratio `[-1, 1]`), discrete actions (position bins), multi-objective rewards and vectorized parallel environment `VecEnv`.

6. **axon-inference**: Model inference engine. Supports ONNX Runtime, Candle (pure Rust), tch-rs (PyTorch C++) three backends, with async batch inference pipeline, CPU/GPU affinity binding and model hot update capability.

7. **axon-llm**: Large language model agent. Based on ReAct reasoning loop, built-in "market analysis", "query portfolio", "place order" three tools, supports OpenAI-compatible backends and streaming responses.

8. **axon-explain**: Explainability engine. Integrates SHAP feature attribution, counterfactual explanations ("What if I hadn't bought, how would returns change") and structured decision reports, meeting compliance and strategy iteration needs.

9. **axon-ensemble**: Model ensemble. Provides HardVote, SoftVote, WeightedVote, Stacking, DynamicWeighted five strategies, supports online performance monitoring and automatic weight adjustment.

---

## Repository Structure

```
axon_quant/
├── crates/                     # 21 Rust crates
│   ├── axon-core/              # Core types (time/types/market/order/event/queue/portfolio)
│   ├── axon-backtest/          # Backtesting engine (L1/L2/L3 matching + impact models)
│   ├── axon-rl/                # RL environment (Gymnasium + VecEnv)
│   ├── axon-hpo/               # Hyperparameter optimization (Optuna + NSGA-II)
│   ├── axon-walk-forward/      # Rolling forward validation (Purged + Embargo)
│   ├── axon-distributed/       # Distributed training (Ray)
│   ├── axon-tracker/           # Experiment tracking (MLflow/WandB/Local/Memory)
│   ├── axon-registry/          # Model registry (SemVer + lifecycle)
│   ├── axon-exchange/          # Exchange adapters (Binance/OKX)
│   ├── axon-inference/         # Inference engine (ONNX/Candle)
│   ├── axon-risk/              # Risk engine
│   ├── axon-oms/               # Order management system
│   ├── axon-monitor/           # Monitoring alerts
│   ├── axon-llm/               # LLM agent
│   ├── axon-python/            # Python bindings entry
│   └── axon-cli/               # CLI tool
├── python/                     # Python package (axon_quant)
├── examples/                   # Training example scripts
├── tests/                      # Tests (Rust + Python)
├── docs/                       # Design docs + ADR
├── scripts/                    # Build and test scripts
├── pyproject.toml              # Python packaging config
├── Makefile                    # Development commands
└── Dockerfile                  # Multi-stage build
```

---

## Crate Matrix

| Crate | Function |
|-------|----------|
| axon-core | Core types (11 modules) |
| axon-backtest | Backtesting engine (L1/L2/L3) |
| axon-rl | RL environment (Gymnasium + VecEnv) |
| axon-hpo | Hyperparameter optimization (Optuna) |
| axon-walk-forward | Rolling forward validation |
| axon-distributed | Distributed training (Ray) |
| axon-tracker | Experiment tracking |
| axon-registry | Model registry |
| axon-exchange | Exchange adapters (Binance/OKX) |
| axon-inference | Inference engine (ONNX/Candle) |
| axon-python | Python bindings (PyO3) |
| axon-cli | CLI tool |
| axon-risk | Risk engine |
| axon-oms | Order management |
| axon-monitor | Monitoring alerts |
| axon-llm | LLM agent |
| axon-explain | SHAP explainability |
| axon-ensemble | Model ensemble |
| axon-compliance | Compliance audit |
| axon-data | Data services |
| axon-integration-tests | Integration tests |

---

## Performance

| Metric | Value |
|--------|-------|
| Backtesting Throughput | > 1M events/sec |
| Matching Latency | < 1μs (P99) |
| Risk Check | 12ns (AtomicBool circuit breaker + HashMap position) |
| Order Submission | 1.2μs (idempotent + UUID v7 + state machine) |
| RL Training | > 10K steps/sec (8 env VecEnv) |
| Distributed Speedup | > 5x (8 workers) |
| Test Cases | 1200+ Rust + 24 Python |

### Benchmarks

Workspace has established 50+ Criterion benches across 5 crates:

| Crate | Bench Entry | Coverage |
|-------|-------------|----------|
| `axon-core` | `benches/core_bench.rs` | 28: impact model/volatility/latency/order book/order/event/fee |
| `axon-backtest` | `benches/impact_bench.rs` | 8: matching latency/impact models/order book depth/permanent decay/multi-fill/TOML config |
| `axon-data` | `benches/axon_data_bench.rs` | 7 groups (8+ bench): LRU/Dataset lazy/CSV/Parquet streaming/Bar aggregation/Mock/Mmap |
| `axon-rl` | `benches/rl_bench.rs` | 11: observation/reward/TradingEnv end-to-end/Action conversion |
| Phase 4 crates | `benches/phase4_bench.rs` | 15: risk/OMS/monitoring latency |

```bash
make bench                 # Full workspace, 5-10 minutes locally
make bench-cmp             # Save main baseline for PR comparison
make bench-one CRATE=axon-core BENCH=event_builder_tick   # Single bench
cargo bench -p axon-core -- impact_linear    # Direct cargo run
```

CI doesn't run bench (to avoid main runner performance noise). Report: `target/criterion/<group>/report/index.html`.

### CPU/GPU Affinity

`axon-inference` provides `affinity` module for cross-platform core pinning to reduce cross-core cache misses:

```rust
use axon_inference::affinity::{AffinityPlan, pin_to};
let plan = AffinityPlan::new().with_cpus(vec![0, 1]).with_cuda(0);
pin_to(&plan)?;
```

Or via `BatchConfig` configuration (auto-called at `BatchInferencePipeline::new` startup):

```toml
[batch]
collect_cpu_cores = [0, 1, 2, 3]
collect_gpu_device_id = 0
```

Platform support: Linux / macOS full support, Windows runtime returns `Err(AffinityError::NotAvailable)` (use WSL2 / numactl instead).

---

## Engineering Practices

- **TDD Driven** — Test first, implement later, CI enforces `-D warnings`
- **1200+ Tests** — Unit tests + integration tests + Python scenario tests
- **cargo clippy** — Zero warning policy
- **cargo-mutants** — Mutation test coverage
- **cargo-fuzz** — Fuzz testing (matching engine / order book / risk control)
- **Miri** — Data race detection
- **Loom** — Deterministic concurrency testing

---

## Documentation

- [Installation & Quick Start](docs/en/getting-started/installation.md)
- [AI-Native Core Design](docs/en/user-guide/ai-native-design.md)
- [Strategy Development Pipeline](docs/en/user-guide/strategy-development.md)
- [LLM Agent Trading](docs/en/user-guide/llm-trading/oader.md)
- [Production Deployment](docs/en/user-guide/production.md)
- [Traditional Strategy Migration](docs/en/user-guide/traditional-strategy.md)
- [API Reference](docs/en/reference/api-reference.md)
- [FAQ](docs/en/about/faq.md)

---

## License

[Apache-2.0](./LICENSE)

---

## Disclaimer

This project is an **open-source quantitative trading framework** intended for **research and educational purposes only**.

- **No investment advice**: Nothing in this repository constitutes financial, investment, or trading advice.
- **No guarantee of profits**: Past performance (including backtesting results) does not guarantee future returns.
- **Use at your own risk**: The authors and contributors are **not responsible for any financial losses** incurred through the use of this software.
- **Not production-ready**: This software is provided "as is" without warranty of any kind. Use in live trading environments requires thorough testing and risk assessment.
- **Regulatory compliance**: Users are solely responsible for ensuring compliance with applicable laws and regulations in their jurisdiction.

**By using this software, you acknowledge that you understand and accept these terms.**
