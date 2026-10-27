# AXON Quant

> **AI-Native Quantitative Trading Framework** — Rust core with Python bindings, a complete pipeline from backtesting to production.

AXON (**A**I-driven e**X**ecution and **O**rder e**N**gine) is an event-driven trading engine designed for quantitative trading and reinforcement learning. It was built from the ground up with AI at its core, rather than "bolting on" machine learning modules to a traditional quant system.

!!! note "Version Info"
    This documentation is based on AXON `v0.22.1`, targeting Rust version `1.97.0+`.

---

## Core Features

<div class="grid cards" markdown>

-   :material-robot-outline: **AI-Native RL Environment**

    ---

    Built-in Gymnasium-compatible `TradingEnv` with discrete/continuous/mixed action spaces and PnL/Sharpe/Sortino reward functions out of the box.

-   :material-lightning-bolt: **Rust High-Performance Core**

    ---

    Nanosecond timestamp precision, L1/L2/L3 deterministic matching, SIMD-accelerated normalization, P99 matching latency < 1μs.

-   :material-source-branch: **Unified Full Pipeline**

    ---

    Backtesting, training, hyperparameter optimization, walk-forward validation, experiment tracking, and model registry share the same `MarketBar` / `PortfolioState` data structures.

-   :material-package-variant-closed: **26 Independent Crates**

    ---

    Each crate can be compiled and published independently, enabled via feature flags. From minimal core `axon-core` to full production stack `axon-exchange`.

-   :material-brain: **LLM + RL Complementary**

    ---

    `axon-llm` provides ReAct agents with tool calling; `axon-rl` provides high-frequency strategy training. Integrated via `axon-ensemble` for "intuition + reasoning" dual engines.

-   :material-eye-outline: **Built-in Explainability**

    ---

    `axon-explain` integrates SHAP feature attribution, counterfactual explanations, and decision report generation for compliance and strategy iteration.

</div>

---

## Design Philosophy

- **AI First**: RL environment and backtesting engine share the same data structures, zero difference between training and production
- **Rust Core**: Nanosecond timestamps, deterministic matching, zero-cost abstractions, backtesting throughput > 1M events/sec
- **Python Front**: Gymnasium-compatible interface via PyO3, directly compatible with Stable-Baselines3 / Ray RLlib
- **Full Pipeline**: Backtest → Train → HPO → Walk-forward → Track → Register → Deploy, all built-in
- **100% Open Source**: Apache-2.0 license, no enterprise edition, no feature restrictions

---

## AI-Native vs Traditional Quantitative

| Dimension | Traditional Quant Framework | AXON (AI-Native) |
|-----------|---------------------------|------------------|
| **Data Pipeline** | CSV/DataFrame manual assembly, inconsistent training/production formats | Arrow `RecordBatch` unified columnar storage, zero-copy `fit`/`transform` pipeline |
| **Strategy Writing** | Rule expressions or standalone scripts | RL strategy = neural network weights + environment interaction; rule strategies also supported via `ActionDecoder` |
| **Backtest vs Live** | Two separate codebases, often "backtest holy grail, live losses" | `TradingEnv` directly calls `axon-backtest` matching engine; swap `ExchangeAdapter` for live trading |
| **Hyperparameter Optimization** | External scripts loosely coupled | `axon-hpo` built-in Optuna + NSGA-II multi-objective + Pareto frontier + early stopping |
| **Explainability** | Post-hoc analysis, manual Jupyter plotting | `axon-explain` computes SHAP values in real-time during `step()`, generates `ExplanationReport` |
| **Model Deployment** | Manual ONNX/TorchScript export + C++ service wrapping | `axon-inference` supports ONNX/Candle/tch backends, batch inference pipeline + hot update |
| **Multi-Model Collaboration** | No built-in support | `axon-ensemble` provides HardVote/SoftVote/WeightedVote/Stacking/DynamicWeighted strategies |
| **Exchange Integration** | Each exchange SDK independently wrapped | `ExchangeAdapter` trait unifies REST + WebSocket, covers Binance/OKX |

---

## Architecture Overview

AXON uses Cargo Workspace to manage **26 crates**, organized by responsibility and dependency topology into **7 layers** (cargo metadata verified):

```
┌──────────────────────────────────────────────────────────┐
│  Layer 6 Application Entry (top-level, depends on all)    │
│  ├─ axon-python         PyO3 unified entry (axon_quant pkg) │
│  └─ axon-integration-tests End-to-end integration gating  │
├──────────────────────────────────────────────────────────┤
│  Layer 5 Strategy & Orchestration                          │
│  ├─ axon-worker         Orchestration: PyKernel / LiveDriver /│
│  │                       PaperDriver / DataPump / Gateway  │
│  └─ axon-strategy       Strategy decision / registration /│
│                         scheduling                          │
├──────────────────────────────────────────────────────────┤
│  Layer 4 LLM Agent                                         │
│  └─ axon-llm            ReAct agent + Tool Calling          │
├──────────────────────────────────────────────────────────┤
│  Layer 3 Unified Trading Kernel (new in 0.22.0, shared     │
│                               by backtest/paper/live)       │
│  └─ axon-kernel         Kernel main loop + BarLifecycle    │
│                         nine steps + ExecutionSink +        │
│                         Journal event-sourcing              │
├──────────────────────────────────────────────────────────┤
│  Layer 2 Pipeline & Governance                              │
│  ├─ axon-rl / axon-hpo / axon-walk-forward  Training pipeline│
│  ├─ axon-tracker / axon-registry  Experiment governance    │
│  ├─ axon-distributed / axon-cli  Distributed & CLI          │
│  ├─ axon-defi            DeFi on-chain interaction          │
│  ├─ axon-risk / axon-monitor  Risk control & monitoring      │
├──────────────────────────────────────────────────────────┤
│  Layer 1 Engine Layer (built directly on axon-core)        │
│  ├─ axon-backtest       L1/L2/L3 matching + isolated margin│
│                         engine                               │
│  ├─ axon-exchange       Binance / OKX adapters (user-data  │
│                         stream + three-pass reconciliation) │
│  ├─ axon-oms            Order management system              │
│  ├─ axon-data           Arrow columnar storage + feature     │
│                         pipeline                             │
│  ├─ axon-inference      ONNX / Candle / tch inference engine│
│  ├─ axon-explain        SHAP explainability                 │
│  ├─ axon-ensemble       HardVote / SoftVote / Stacking      │
│  ├─ axon-compliance     Compliance audit + daily / monthly  │
│                         reports                              │
│  └─ axon-harness        Unified benchmark / conformance      │
├──────────────────────────────────────────────────────────┤
│  Layer 0 Core Types (bottom layer, depended on by all)     │
│  └─ axon-core           Timestamp / Price / Order /          │
│                         Instrument / Event / Queue / Fees   │
│                         / SIMD                               │
└──────────────────────────────────────────────────────────┘
```

---

## Performance Metrics

| Metric | Value |
|--------|-------|
| Backtesting Throughput | > 1,000,000 events/sec |
| Matching Latency (P99) | < 1 μs |
| RL Training (8 env VecEnv) | > 10,000 steps/sec |
| Distributed Speedup (8 workers) | > 5x |
| Test Cases | 1200+ Rust + 24 Python |

---

## Quick Start

```python
import axon_quant

env = axon_quant.rl.TradingEnv(
    config={"initial_capital": 100_000.0, "max_steps": 500},
    market_data=bars,
    action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    reward="sharpe",
)

obs = env.reset()
obs, reward, terminated, truncated, info = env.step([0.5])
```

---

## Documentation

- [Installation & Quick Start](getting-started/installation.md)
- [Quick Start](getting-started/quickstart.md)
- [Architecture Overview](user-guide/architecture.md)
- [AI-Native Core Design](user-guide/ai-native-design.md)
- [Strategy Development](user-guide/strategy-development.md)
- [LLM Agent Trading](user-guide/llm-trading/oader.md)
- [Production Deployment](user-guide/production.md)
- [Traditional Strategy Migration](user-guide/traditional-strategy.md)
- [Module Reference (26 crates)](user-guide/modules.md)
- [API Reference](reference/api-reference.md)
- [FAQ](about/faq.md)

---

## Disclaimer

This project is an **open-source quantitative trading framework** for **research and educational purposes only**. The authors and contributors are **not responsible for any financial losses** incurred through the use of this software. By using this software, you acknowledge that you understand and accept these terms. See [LICENSE](https://github.com/pengwow/axon_quant/blob/main/LICENSE).
