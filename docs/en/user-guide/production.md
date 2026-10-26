# Scenario 3 — Production Deployment and Monitoring

> **Related examples**:
> - [`examples/09_exchange/binance_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/09_exchange/binance_demo.py) — Exchange integration
> - [`examples/12_inference/inference_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/12_inference/inference_demo.py) — Inference engine
> - [`examples/14_ensemble/ensemble_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/14_ensemble/ensemble_demo.py) — Ensemble learning
> - [`examples/15_explain/explain_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/15_explain/explain_demo.py) — Explainability

This document covers the complete process of moving AXON quantitative trading framework from laboratory to production, including inference backend selection, model hot updates, explainability auditing, model ensemble dynamic weighting, and exchange integration.

---

## 1. Inference Engine Three-Backend Selection Guide

AXON's inference engine (`axon-inference`) supports three backends, each suited for different deployment scenarios.

### 1.1 Backend Comparison Table

| Dimension | ONNX | tch (PyTorch C++) | Candle (Pure Rust) |
|-----------|------|-------------------|-------------------|
| **Dependencies** | `ort` (ONNX Runtime) | `tch-rs` (LibTorch) | `candle-core` + `candle-nn` |
| **Binary Size** | Medium (+ ONNX Runtime) | Large (+ LibTorch) | Small (Pure Rust) |
| **Startup Speed** | Fast | Medium | Very Fast |
| **CPU Inference Latency** | < 500µs | < 1ms | < 500µs |
| **GPU Support** | CUDA / TensorRT | CUDA / ROCm | CUDA (Experimental) |
| **Model Format** | `.onnx` | `.pt` / `.torchscript` | `.safetensors` |
| **Hot Update Support** | `replace_session` | `replace_session` | `load(new_path)` |
| **Use Case** | Production preferred | Research/fast iteration | Minimal deployment without Python |

### 1.2 Backend Selection Decision Tree

```text
Need GPU acceleration?
├── Yes → Need TensorRT?
│   ├── Yes → ONNX (Level3 optimization + TensorRT EP)
│   └── No → tch (CUDA) or ONNX (CUDA EP)
└── No → Mind Python dependency?
    ├── Yes → Candle (pure Rust, zero Python)
    └── No → ONNX (CPU, most mature ecosystem)
```

### 1.3 Configuration Example

```python
from axon_quant import InferenceBackend, Device, ModelConfig

# ONNX production configuration
onnx_config = ModelConfig(
    path="models/production.onnx",
    backend=InferenceBackend.ONNX,
    device=Device.CUDA(0),          # Use first GPU
    input_shape=[1, 64, 128],       # [batch, seq_len, features]
    output_dim=3,                   # Buy / Sell / Hold
    fp16=True,                      # Enable FP16 inference
    num_threads=4,                  # ONNX Runtime threads
)

# Candle zero-dependency configuration
candle_config = ModelConfig(
    path="models/production.safetensors",
    backend=InferenceBackend.CANDLE,
    device=Device.CPU,
    input_shape=[1, 4, 1],          # input_dim = 1*4*1 = 4
    output_dim=3,
    fp16=False,                     # Candle doesn't support FP16 yet
    num_threads=4,
)
```

---

## 2. Model Hot Update

In production environments, models need to be updated without restarting services. AXON achieves atomic replacement via `ModelHotReloader` + `notify` file monitoring.

### 2.1 Core Mechanism

```text
File system monitoring (notify)
       │
       ▼
Detect model file change
       │
       ▼
Debounce processing (500ms aggregation of consecutive events)
       │
       ▼
Calculate new model SHA256 checksum
       │
       ▼
Acquire backend write lock → load new model → release write lock
       │
       ▼
Atomic version increment → broadcast via watch channel
```

### 2.2 Hot Update Code Example

```python
import asyncio
from axon_quant import ModelHotReloader, OnnxBackend, ModelConfig

async def setup_hot_reload():
    """
    Configure model hot update system.
    
    When the model file changes, automatically:
    1. Verify SHA256 checksum
    2. Acquire write lock
    3. Load new model
    4. Broadcast version change
    """
    config = ModelConfig(
        path="models/production.onnx",
        backend=InferenceBackend.ONNX,
        device=Device.CUDA(0),
    )
    
    engine = OnnxBackend(config)
    engine.load(Path(config.path))
    
    reloader = ModelHotReloader(engine, config)
    
    # Start file watcher
    reloader.spawn_watcher()
    
    # Subscribe to version changes
    version_rx = reloader.subscribe()
    
    async def watch_versions():
        while True:
            await version_rx.changed()
            version = version_rx.borrow()
            print(f"Model updated to version {version}")
    
    asyncio.create_task(watch_versions())
    
    return reloader
```

---

## 3. Explainability Audit

AXON provides built-in explainability via `axon-explain`, supporting SHAP feature attribution, counterfactual explanations, and decision reports.

### 3.1 SHAP Feature Attribution

```python
from axon_quant.explain import KernelSHAP

explainer = KernelSHAP(model)

# Explain a single prediction
explanation = explainer.explain(
    observation=obs,
    action=predicted_action,
    background_data=background_samples,
)

# Visualize feature importance
print("Feature importance:")
for feature, importance in sorted(
    zip(explanation.feature_names, explanation.feature_importances),
    key=lambda x: abs(x[1]),
    reverse=True,
):
    print(f"  {feature}: {importance:.4f}")
```

### 3.2 Counterfactual Explanations

```python
from axon_quant.explain import CounterfactualGenerator

generator = CounterfactualGenerator(model)

# Generate counterfactual: "What if the action was different?"
counterfactuals = generator.generate(
    observation=obs,
    original_action=original_action,
    n_samples=100,
)

# Analyze: "What would need to change for a different outcome?"
for cf in counterfactuals:
    print(f"Change {cf.feature_name} by {cf.delta:.4f}")
    print(f"  Original: {cf.original_value:.4f}")
    print(f"  Counterfactual: {cf.counterfactual_value:.4f}")
```

### 3.3 Decision Report Generation

```python
from axon_quant.explain import ReportGenerator

generator = ReportGenerator(model)

# Generate comprehensive decision report
report = generator.generate_report(
    observation=obs,
    action=predicted_action,
    include_shap=True,
    include_counterfactuals=True,
    include_feature_importance=True,
)

# Export as HTML/PDF
report.export_html("decision_report.html")
report.export_pdf("decision_report.pdf")
```

---

## 4. Model Ensemble Dynamic Weighting

AXON's `axon-ensemble` module provides dynamic weight adjustment based on real-time performance monitoring.

### 4.1 DynamicWeightedEnsemble

```python
from axon_quant.ensemble import DynamicWeightedEnsemble

# Create ensemble with multiple models
ensemble = DynamicWeightedEnsemble(
    models=[ppo_model, sac_model, rule_based_model],
    initial_weights=[0.4, 0.4, 0.2],
    performance_window=100,  # Last 100 trades for weight calculation
)

# Update weights based on performance
ensemble.update_weights(
    performances=[ppo_sharpe, sac_sharpe, rule_sharpe]
)

# Get weighted prediction
action = ensemble.predict(observation)
```

### 4.2 Performance Monitoring

```python
# Monitor ensemble performance
metrics = ensemble.get_metrics()

print(f"Ensemble Sharpe: {metrics['sharpe']:.2f}")
print(f"Model weights: {ensemble.weights}")
print(f"Active models: {metrics['active_count']}")
```

---

## 5. Exchange Integration

AXON provides production-ready exchange adapters for Binance and OKX.

### 5.1 Binance Adapter Configuration

```python
from axon_quant.exchange import BinanceAdapter, ExchangeConfig

config = ExchangeConfig(
    api_key="YOUR_API_KEY",
    api_secret="YOUR_API_SECRET",
    testnet=False,  # Production
    rate_limit=RateLimitConfig(
        requests_per_second=10,
        orders_per_minute=60,
    ),
    reconnect=ReconnectConfig(
        max_retries=10,
        initial_backoff_ms=500,
    ),
)

adapter = BinanceAdapter(config)
await adapter.connect()
```

### 5.2 Production Order Flow

```python
# Place order with risk checks
# 0.6.0 Python adapter.place_order accepts only dict, not Order instance
order = {
    "symbol": "BTCUSDT",
    "side": "buy",
    "type": "limit",
    "price": "50000",
    "quantity": "0.001",
    "tif": "GTC",
}

# Risk check before submission
if risk_engine.check_order(order):
    order_id = await adapter.place_order(order)
    print(f"Order placed: {order_id}")
else:
    print("Order rejected by risk engine")
```

### 5.3 Crash Recovery and Data Consistency (v0.22.0+)

The most lethal production risk is a process crash or host reboot that leaves in-kernel OMS state unpersisted. AXON v0.22.0 ships an append-only journal + replay-gate crash-recovery chain. Key components (each name is a real symbol from the Rust source):

| Component | Responsibility | Source Location |
|-----------|----------------|-----------------|
| `axon_kernel::journal::Journal` | Append-only JSONL persistence; first line is a `RunManifest` (replay fingerprint: code version, initial cash, symbols, contract sizes); every subsequent line is `JournalEntry { event, state }` | `crates/axon-kernel/src/journal.rs` |
| `RunManifest` | Replay fingerprint — any field mismatch aborts replay immediately; always the first line of the journal | `crates/axon-kernel/src/persist.rs` |
| `LiveDriver.start(journal_path)` | On first run calls `Journal::create(path, manifest)` and appends every input event that enters the kernel | `crates/axon-worker/src/python.rs` |
| `LiveRecovery.new(journal_path)` + `.replay(...)` | Recovery entry point: internally calls `Journal::resume(path)` (tolerates trailing half-line, rejects any damaged line in the middle), replays every recorded `JournalEntry` back into the kernel, then runs 3-way reconciliation and gates the chain with `resume_guard` | `crates/axon-worker/src/python.rs` |
| `KernelPump` gap backfill | After WS reconnect, `KernelPump::replay_bars` pulls historical K-lines from the exchange to fill the missing bar window so the replayed bar sequence stays aligned with what was recorded in the journal | `crates/axon-worker/src/pump.rs` |

Recovery sequence (pseudo-code):

```python
import axon_worker

# 1) Read the RunManifest header, verify code version / config match
manifest = axon_worker.read_manifest(journal_path)

# 2) Replay all recorded JournalEntry events back into the kernel
rec = axon_worker.LiveRecovery(initial_cash=1_000_000.0)
report = rec.replay(journal_path, reconcile_with_exchange=True)

# 3) resume_guard: any break raises, blocking the live chain; all-green only
axon_worker.resume_guard(report)

# 4) Reconnect the WS + KernelPump, keep appending to the same journal
live = axon_worker.LiveDriver(...)
live.start(journal_path)
```

Design constraints worth knowing before relying on this in production:

- The journal is append-only with **strict** integrity checks: any mid-file line that fails `serde_json` parsing, misses the required `event` / `state` keys, or is written by a newer version than `JOURNAL_FORMAT_VERSION` gets rejected — no silent skipping;
- `RunManifest.code_version` must equal the currently running `axon_quant.__version__`, otherwise `LiveRecovery.replay` aborts (kernel state-machine semantics can drift across versions);
- After replay, kernel state ≈ state after the **last complete `JournalEntry`** before the crash — any half-written trailing line is truncated and discarded, intentionally preventing replay from driving the OMS into an undefined half-applied intermediate state.

## 6. Three-Way Reconciliation (v0.22.0+)

On restart or WS reconnect, the OMS ledger can drift away from the exchange's real state. AXON v0.22.0 wires a 3-way check inside `LiveSink`: **positions / fills / cash**, with the following guarantees:

- Per-symbol position diff larger than threshold → **frozen for opening** on that symbol; closing existing positions is still allowed
- `fetch_my_trades` + the latest dedup `ORDER_TRADE_UPDATE` stream must line up with locally recorded fills; unknown external fills (manual orders, history that never arrived as a push) are reported as reconcile breaks
- Cash baseline uses the wallet total + available balance returned by the exchange (mode `AccountingMode::Cash` vs. `Wallet` is declared when constructing `LiveSink`)
- `resume_guard` **reports but does not auto-fix**: any break aborts the live chain. The engine never silently rewrites OMS state to "match the exchange" — if reconciliation fails, a human must intervene
- Open-order reconciliation (fixed in v0.22.0): on recovery the engine pulls `open_orders` from the exchange and backfills the OMS; unknown open orders or manual orders are surfaced as breaks (no more false "phantom diff" reports)

---

## 7. Monitoring and Alerting

### 7.1 Metrics Collection

```python
from axon_quant.metrics import MetricsCollector

collector = MetricsCollector()

# Record trading metrics
collector.record_order(side="buy", symbol="BTCUSDT", quantity=0.001)
collector.record_latency(operation="place_order", duration_ms=45.2)
collector.record_pnl(pnl=150.0, symbol="BTCUSDT")

# Export to Prometheus
collector.export_prometheus(port=9100)
```

### 7.2 Alert Rules

```yaml
# Example Prometheus alert rules
groups:
  - name: trading_alerts
    rules:
      - alert: HighLatency
        expr: trading_order_latency_seconds > 0.5
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Trading order latency is high"
      
      - alert: LargeDrawdown
        expr: trading_max_drawdown > 0.1
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "Portfolio drawdown exceeds 10%"
```

---

## 8. Deployment Checklist

### 8.1 Pre-Deployment

- [ ] Run full test suite: `cargo test --workspace`
- [ ] Verify configuration: `axon validate-config -c production.toml`
- [ ] Load test with simulated traffic
- [ ] Set up monitoring and alerting
- [ ] Configure backup and rollback procedures
- [ ] Run crash-recovery rehearsal: `Journal.resume` + `LiveRecovery.replay` against journal captured on testnet, confirm OMS state matches the exchange after replay

### 8.2 Deployment

- [ ] Deploy to staging environment first
- [ ] Run integration tests against staging
- [ ] Gradual rollout (canary deployment)
- [ ] Monitor metrics for anomalies
- [ ] Keep previous version ready for rollback

### Post-Deployment

- [ ] Verify all trading pairs are operational
- [ ] Check latency metrics are within SLA
- [ ] Monitor error rates
- [ ] Review PnL and position metrics
- [ ] Document any issues encountered

---

## Next Steps

- [Operations Runbook](llm-trading/operations-runbook.md) — Deployment and troubleshooting
- [Risk & Safety](llm-trading/risk-safety.md) — Risk control configuration
- [Metrics & Alerting](llm-trading/metrics-alerting.md) — Monitoring setup
