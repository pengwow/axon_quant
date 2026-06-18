# AI-Native Core Design

> AXON is not a "traditional quant framework + AI plugin" but a unified framework redesigned from data pipeline to production deployment for AI workflows. This chapter provides an in-depth analysis of its four design pillars, module integration mechanisms, and unified data pipeline.

---

## What is AI-Native

"AI-Native" is not a marketing term but four fundamental architectural design decisions in AXON:

### 1. Unified Data Pipeline: Training and Production Share the Same Arrow Columnar Data

In traditional quant systems, researchers use Pandas DataFrame for feature engineering, while engineers use Protobuf or custom formats for live trading. The "format conversion layer" between them is a hotspot for bugs and information loss.

AXON's `axon-data` uses **Apache Arrow `RecordBatch`** as the sole in-memory representation at the lowest level:

```rust
// axon-data/src/pipeline.rs
// FeaturePipeline performs fit + transform on Dataset, fully columnar zero-copy

pub trait Normalizer: Send + Sync {
    /// Training phase: learn normalization parameters from dataset
    fn fit(&mut self, ds: &Dataset);

    /// Inference phase: convert dataset to FeatureMatrix
    fn transform(&self, ds: &Dataset) -> FeatureMatrix;
}

/// Z-Score normalizer: (x - mean) / std
pub struct ZScoreNormalizer {
    mean: f64,
    std: f64,
}
```

- **During training**: `fit_transform()` learns mean/variance from historical data, outputs `FeatureMatrix` for neural network consumption
- **During production**: Same `transform()` path, uses saved `mean`/`std` from training, ensuring distribution consistency
- **Zero-copy**: Arrow columnar buffer passes directly to SIMD normalization, avoiding intermediate `Vec<Tick>` representation

!!! note "Why Arrow"
    Arrow's columnar memory layout and zero-copy characteristics enable the Rust kernel, Python training scripts, and ONNX inference engine to share the same memory block without serialization/deserialization overhead.

### 2. Same Code for Training and Production: TradingEnv's Core is the Backtesting Engine

In traditional frameworks, backtesting uses one set of Python scripts while live trading uses another C++ service, requiring strategy logic to be "translated" twice.

AXON's `TradingEnv` directly wraps `axon-backtest`'s matching engine:

```rust
// axon-rl/src/env/trading_env.rs
// TradingEnv::step() internally calls Executor::execute(), sharing the same order book logic with backtesting engine

pub fn step(&mut self, action: &Action) -> EnvResult<StepResult> {
    // 1. Action → Order (ActionDecoder unified discrete/continuous action parsing)
    let order = self.decoder.decode(action, &self.portfolio)?;

    // 2. Execute order → Underlying Backtest matching engine (with impact model and slippage)
    if let Some(o) = order {
        let results = self.executor.execute(&[o], &current_bar, &mut self.portfolio)?;
        for r in &results {
            if r.filled {
                self.trades_executed += 1;
                self.transaction_costs += r.cost;
            }
        }
    }

    // 3. Revalue portfolio based on next K-line close
    self.executor.revalue(&mut self.portfolio, next_bar.close)?;

    // 4. Calculate reward (PnL / Sharpe / Sortino share the same ReturnHistory)
    let reward = self.reward_fn.calculate(...)?;

    Ok((obs, reward, self.done, info))
}
```

**For live trading, only `ExchangeAdapter` needs to be swapped**:

```rust
// axon-exchange/src/traits.rs
/// Unified exchange interface: Binance / OKX both implement this trait
pub trait ExchangeAdapter: Send + Sync {
    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck, ExchangeError>;
    async fn cancel_order(&self, id: &OrderId) -> Result<(), ExchangeError>;
    async fn query_portfolio(&self) -> Result<PortfolioSnapshot, ExchangeError>;
    async fn subscribe_market_data(&self, symbols: &[Symbol]) -> Result<DataStream, ExchangeError>;
}
```

Strategy code (`reset` / `step` / `render`) remains completely unchanged, achieving **zero difference between training and production**.

### 3. LLM + RL Complementary: "Intuition Engine" + "Reasoning Engine" Dual-Mode Decision Making

AXON supports both AI paradigms natively, integrated through `axon-ensemble`:

| Capability | RL (`axon-rl`) | LLM (`axon-llm`) |
|-----------|----------------|------------------|
| **Decision Method** | Pattern recognition + statistical optimization | Symbolic reasoning + natural language understanding |
| **Strengths** | High-frequency microstructure, price prediction | Macro event interpretation, earnings analysis, anomaly detection |
| **Input** | Normalized feature vectors | Text context (news, announcements, on-chain data) |
| **Output** | Continuous position / discrete actions | Structured tool calls (order / query / analysis) |
| **Explainability** | Requires SHAP post-hoc attribution | Chain-of-Thought reasoning is naturally interpretable |

```rust
// axon-llm/src/trading/mod.rs
// LLM trading tools: place_order / query_portfolio, with SafetyMode risk control

pub use place_order_tool::PlaceOrderTool;
pub use query_portfolio_tool::QueryPortfolioTool;
pub use safety::{DailyCounter, RiskLimits, SafetyMode};
```

`axon-ensemble`'s `DynamicWeightedEnsemble` monitors RL and LLM sub-model Sharpe ratios in real-time, dynamically adjusting weights:

```rust
// axon-ensemble/src/dynamic.rs
// Online performance monitoring + automatic weight adjustment

pub struct DynamicWeightedEnsemble {
    models: Vec<Box<dyn Policy>>,
    weights: Vec<f64>,
    performance_window: VecDeque<f64>,
}

impl Ensemble for DynamicWeightedEnsemble {
    fn update_weights(&mut self, performances: &[f64]) {
        // Decay low-performance model weights based on recent Sharpe ratios
        // ...
    }
}
```

### 4. Built-in Explainability: SHAP + Counterfactual + Decision Reports, Not Post-Hoc Patches

In traditional frameworks, explainability is a "figure it out after training" afterthought. AXON defines `Explainer` as a core trait at the same level as `Policy`:

```rust
// axon-explain/src/traits.rs
/// Explainer trait: generates complete explanations for a model decision
pub trait Explainer: Send + Sync {
    /// Explains a complete decision (feature attribution + counterfactual + attention visualization)
    fn explain(
        &self,
        observation: &HashMap<String, f64>,
        action: &ActionSnapshot,
    ) -> Result<Explanation, ExplainabilityError>;

    /// Generates counterfactual explanations: "How would returns change if I hadn't bought"
    fn generate_counterfactuals(
        &self,
        observation: &HashMap<String, f64>,
        action: &ActionSnapshot,
        max_changes: usize,
    ) -> Vec<CounterfactualExplanation>;
}
```

Every `step()` decision can simultaneously generate an `ExplanationReport`, archived with model versions to `axon-registry` for compliance audit requirements.

---

## Module Integration Matrix

AXON's 6 AI core modules are not isolated; they form tight integration through traits and shared types:

| | **RL** | **LLM** | **Inference** | **Explain** | **Ensemble** | **Exchange** |
|:---|:---|:---|:---|:---|:---|:---|
| **RL** | — | LLM as `Policy` connects to `Ensemble` | `InferenceEngine` provides model predictions for `TradingEnv` | `Explainer` explains `TradingEnv`'s every step decision | `VecEnv` parallel rollout for `Ensemble` evaluation | `TradingEnv`'s underlying matching logic aligns with `ExchangeAdapter` |
| **LLM** | RL strategies as Tools for LLM to call | — | `InferenceEngine` accelerates LLM backend (local models) | `explain` module generates attribution for LLM decisions | LLM output fused with RL strategies via `Ensemble` | `PlaceOrderTool` / `QueryPortfolioTool` directly call `ExchangeAdapter` |
| **Inference** | Provides low-latency inference for RL `Policy` | Provides local model (Candle) inference for LLM | — | `Explainer` needs `ModelPredictor` to evaluate counterfactual inputs | `Ensemble` aggregates multiple `InferenceEngine` outputs | Production `InferenceEngine` gets real-time quotes via `ExchangeAdapter` |
| **Explain** | Explains RL strategy's every action | Explains LLM's tool call decisions | Explains model prediction feature importance | — | Generates independent explanation reports for each sub-model in `Ensemble` | Attributes exchange anomalies (e.g., slippage spikes) |
| **Ensemble** | Integrates multiple RL strategies (PPO / SAC / rule-based) | Integrates multiple LLM backends (OpenAI / local) | Integrates ONNX / Candle / tch backend outputs | Aggregates multi-model explanations, generates consistency report | — | `Ensemble`'s final action places orders via `ExchangeAdapter` |
| **Exchange** | Backtest data feeds `TradingEnv` | Real-time quotes feed LLM context | Real-time features feed `InferenceEngine` | Trade records verify explanation accuracy | Trade results feed back to `Ensemble` to update weights | — |

### Integration Example: LLM Senses Macro Events → RL Adjusts Positions

```
┌─────────────┐     Earnings Text     ┌─────────────┐
│  External    │ ──────────────────→  │   axon-llm  │
│  News Source │                      │  (ReAct     │
│  (API/Scrape)│                      │   Reasoning)│
└─────────────┘                      └──────┬──────┘
                                            │ "Suggest reducing position by 30%"
                                            ▼
                              ┌─────────────────────┐
                              │   axon-ensemble      │
                              │ (DynamicWeighted)    │
                              │  RL weight 0.7 → 0.5 │
                              │  LLM weight 0.3 → 0.5│
                              └──────────┬──────────┘
                                         │ Fused action
                                         ▼
                              ┌─────────────────────┐
                              │  axon-inference      │
                              │ (ONNX inference      │
                              │  < 500µs)            │
                              └──────────┬──────────┘
                                         │ Target position
                                         ▼
                              ┌─────────────────────┐
                              │  axon-exchange       │
                              │ (Binance/OKX order)  │
                              └─────────────────────┘
```

---

## Unified Data Pipeline Diagram

All AXON modules share the same data flow, from source to consumer without format conversion gaps:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          Data Source Layer (DataSource)                       │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐     │
│  │ CSV      │  │ Parquet  │  │ WebSocket│  │  Mock    │  │ Exchange │     │
│  │ (Local)  │  │ (Columnar)│  │ (Live)   │  │ (Synth)  │  │ API(REST)│     │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘     │
│       └─────────────┴─────────────┴─────────────┴─────────────┘            │
│                                     │                                      │
│                                     ▼                                      │
│                    ┌────────────────────────────┐                          │
│                    │    axon-data (Unified)      │                          │
│                    │  - Schema validation        │                          │
│                    │  - Time alignment / dedup   │                          │
│                    │  - Columnar cache (mmap)    │                          │
│                    └─────────────┬──────────────┘                          │
│                                  │                                        │
│                                  ▼                                        │
│                    ┌────────────────────────────┐                          │
│                    │   Arrow RecordBatch (Memory)│                          │
│                    │  ┌────────────────────────┐ │                          │
│                    │  │ timestamp │ open │ ... │ │  ← Zero-copy, shared    │
│                    │  └────────────────────────┘ │    across languages     │
│                    └─────────────┬──────────────┘                          │
│                                  │                                        │
└──────────────────────────────────┼────────────────────────────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                          Consumer Layer (Consumers)                          │
│                                                                             │
│   ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐       │
│   │   TradingEnv     │    │  FeaturePipeline │    │  BacktestEngine │       │
│   │  (axon-rl)       │    │  (axon-data)     │    │  (axon-backtest)│       │
│   │                  │    │                  │    │                 │       │
│   │  reset() ──→ obs │    │  fit() / transform│   │  L1/L2/L3       │       │
│   │  step()  ──→ (o,r,d,i)│  → FeatureMatrix │    │  Matching       │       │
│   └────────┬────────┘    └────────┬────────┘    │  + Impact Model │       │
│            │                      │              └─────────────────┘       │
│            │                      ▼                                        │
│            │           ┌─────────────────┐                                 │
│            │           │ InferenceEngine │                                 │
│            │           │ (axon-inference)│                                 │
│            │           │                 │                                 │
│            │           │ ONNX / Candle   │                                 │
│            │           │ Batch < 1ms     │                                 │
│            │           └────────┬────────┘                                 │
│            │                    │                                          │
│            └────────────────────┼──────────────────────────────────────────┘
│                                 │
│                                 ▼
│                    ┌────────────────────────────┐
│                    │      ExchangeAdapter        │
│                    │    (axon-exchange)          │
│                    │  Binance / OKX Live Order   │
│                    └────────────────────────────┘
│
└─────────────────────────────────────────────────────────────────────────────┘
```

### Key Data Flow Node Descriptions

1. **DataSource**: `axon-data`'s `DataSource` trait unifies CSV / Parquet / WebSocket / Mock / Exchange API five data sources. Adding new sources only requires implementing `fetch(&self, req: DataRequest) -> Result<Dataset, DataError>`.

2. **Arrow RecordBatch**: All data sources are converted to Arrow columnar format. `Dataset::iter_batches()` returns `&RecordBatch`, downstream modules read column data via `downcast_ref::<Float64Array>()` without intermediate structure allocation.

3. **TradingEnv**: Extracts `MarketBar` (OHLCV) from `RecordBatch`, initializes portfolio state per `EnvConfig`. `step()` internally decodes actions to orders, calls `Executor` for matching, updates `PortfolioState`.

4. **FeaturePipeline**: Executes `fit_transform()` on `Dataset`, outputs `FeatureMatrix` (`Vec<f32>` row-major). This matrix can be directly fed to ONNX / Candle inference engines or enter `TradingEnv` as `Observation`.

5. **InferenceEngine**: Receives `Observation` (containing `features: Vec<f32>`), processes asynchronously via `BatchInferencePipeline`, returns `Action`. CPU affinity module automatically pins cores to reduce cross-core cache misses.

6. **ExchangeAdapter**: In production, `Ensemble`'s final actions are submitted to Binance / OKX via `ExchangeAdapter::place_order()`. In backtesting, the same actions are matched locally via `BacktestEngine::execute()`.

---

## AI-Native Value Summary

| Traditional Pain Point | AXON's AI-Native Solution | Corresponding Module |
|----------------------|--------------------------|---------------------|
| Training/production data format inconsistency | Arrow `RecordBatch` unified columnar storage, zero-copy passthrough | `axon-data` |
| Backtesting and live engines are two codebases | `TradingEnv` core is backtesting matching, live only swaps `ExchangeAdapter` | `axon-rl` + `axon-backtest` + `axon-exchange` |
| Model unexplainable after training | `Explainer` trait built-in, every step decision sync generates SHAP + counterfactual report | `axon-explain` |
| Single model robustness poor | `Ensemble` supports 5 integration strategies, online monitoring auto-adjusts weights | `axon-ensemble` |
| Hyperparameter optimization loosely coupled | `axon-hpo` native Optuna + NSGA-II integration, directly operates `TradingEnv` | `axon-hpo` |
| Model deployment requires manual export + service wrapping | `axon-inference` three backends + hot update + batch inference, out of the box | `axon-inference` |
| LLM and quant system disconnected | `axon-llm` ReAct agent built-in trading tools, integrates with RL via `Ensemble` | `axon-llm` + `axon-ensemble` |

!!! tip "Core Philosophy"
    AXON's AI-native design does not pursue "using the most cutting-edge models" but rather "enabling the most cutting-edge models to seamlessly integrate into the full quantitative trading pipeline." Data consistency, code consistency, explanation consistency, deployment consistency — this is the fundamental difference between AXON and traditional frameworks.

---

## Next Steps

- [Home](../index.md) — Review AXON's overall positioning and core features
- [Installation & Quick Start](../getting-started/installation.md) — Install and run your first random strategy baseline
- Read `examples/02_rl_training/train_ppo.py` — Experience RL strategy training end-to-end
- Read `crates/axon-llm/examples/integrated_trading_demo.rs` — Experience LLM + RL integrated trading
