# AXON Quant

> **AI-Native Quantitative Trading Framework** — Rust core with Python bindings, a complete pipeline from backtesting to production.

AXON (**A**I-driven e**X**ecution and **O**rder e**N**gine) is an event-driven trading engine designed for quantitative trading and reinforcement learning. It was built from the ground up with AI at its core, rather than "bolting on" machine learning modules to a traditional quant system.

!!! note "Version Info"
    This documentation is based on AXON `v0.10.0`, targeting Rust version `1.96.0+`.

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

-   :material-package-variant-closed: **23 Independent Crates**

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

AXON uses Cargo Workspace to manage 23 crates, organized in 9 layers:

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
- [Module Reference (23 crates)](user-guide/modules.md)
- [API Reference](reference/api-reference.md)
- [FAQ](about/faq.md)

---

## Disclaimer

This project is an **open-source quantitative trading framework** for **research and educational purposes only**. The authors and contributors are **not responsible for any financial losses** incurred through the use of this software. By using this software, you acknowledge that you understand and accept these terms. See [LICENSE](https://github.com/pengwow/axon_quant/blob/main/LICENSE).
