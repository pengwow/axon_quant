# LLM Trading Metrics & Alerting

> Applicable version: axon-llm v0.6.0+
> Prerequisites: [overview.md](overview.md) §5

This document details `TradingMetrics`'s metric system, data outlets, and integration templates for applications to connect to monitoring backends.

**Key Decision**: axon-llm does **not** impose a specific monitoring stack (no built-in Prometheus exporter, no `axon-monitor` dependency, no Grafana dashboard / Prometheus alerting YAML). `TradingMetrics` is self-contained with `Mutex` + `AtomicU64`, applications connect to any monitoring backend (Prometheus / OpenTelemetry / StatsD / custom) via callback / snapshot data outlets.

Grafana dashboards / Prometheus alerting rules are configured by each team per their monitoring stack; axon roadmap does not centrally maintain them.

## 1. Core Metrics (4 Types)

### 1.1 Counter: `trading_orders_total{tool,side,status}`

Increments +1 for each order/cancel/modify per tool. Optional labels:

| Label | Values | Description |
|-------|--------|-------------|
| `tool` | `place` / `cancel` / `replace` | Which tool triggered |
| `side` | `buy` / `sell` / `none` | Order direction (cancel/replace = `none`) |
| `status` | `success` / `rejected` / `failed` | Execution result |

### 1.2 Counter: `trading_risk_rejections_total{source}`

Increments +1 for each risk control rejection. Optional labels:

| Label | Values | Description |
|-------|--------|-------------|
| `source` | `risk_limits` / `risk_gate` / `safety_mode` | Which defense line rejected |

### 1.3 Counter: `trading_backend_errors_total{backend,kind}`

Increments +1 for each backend call failure. Optional labels:

| Label | Values | Description |
|-------|--------|-------------|
| `backend` | `mock` / `exchange` / `oms` / `backtest` | Which backend |
| `kind` | `network` / `rejected` / `timeout` / `other` | Error type |

### 1.4 Histogram: `trading_tool_execute_duration_seconds{tool}`

`Tool::execute()` end-to-end latency distribution, typical buckets: `[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0]` (seconds).

### 1.5 Gauge: `trading_daily_orders_count`

Daily cumulative order count (mirrored from `RiskLimits::DailyCounter`), auto-resets daily at UTC 0:00.

## 2. Data Outlets

### 2.1 Callback (Real-time Push)

```rust
use std::sync::Arc;
use axon_llm::trading::metrics::{TradingMetrics, MetricSample};

let metrics = TradingMetrics::new();

// Register callback: called immediately on each metric change
metrics.set_callback(Arc::new(|sample: MetricSample| {
    match sample {
        MetricSample::CounterInc { name, labels, value } => {
            println!("[counter] {} {:?} += {}", name, labels, value);
            // Push to Prometheus / OTLP / custom sink
        }
        MetricSample::HistogramObserve { name, labels, value_secs } => {
            println!("[histogram] {} {:?} observe {}s", name, labels, value_secs);
        }
        MetricSample::GaugeSet { name, value } => {
            println!("[gauge] {} = {}", name, value);
        }
    }
}));
```

**Notes**:
- Callback holds `Mutex<Option<Arc<dyn Fn ...>>>` inside `TradingMetrics`, only 1 callback allowed
- Callback blocking causes all subsequent metric recording to block — **callbacks must be non-blocking** (use channel / mpsc / coroutine)
- Recommended: push callback work to a separate tokio task

### 2.2 Snapshot (On-Demand Pull)

```rust
let snapshot: Vec<MetricSample> = metrics.snapshot();

for sample in snapshot {
    println!("{:?}", sample);
}
```

**Typical Use Cases**:
- Periodic reporting (every 10s / 30s)
- Health check endpoint returns current metric state
- Integration tests verify metric increments

## 3. Application Integration Examples

### 3.1 Rust + Prometheus (using `prometheus` crate)

```rust
use prometheus::{Registry, IntCounterVec, HistogramVec, IntGauge, register_int_counter_vec_with_registry, register_histogram_vec_with_registry, register_int_gauge_with_registry};

let registry = Registry::new();
let orders_total = register_int_counter_vec_with_registry!(
    "trading_orders_total", "Total trading orders",
    &["tool", "side", "status"], registry
)?;
let duration = register_histogram_vec_with_registry!(
    "trading_tool_execute_duration_seconds", "Tool execution duration",
    &["tool"], registry
)?;

let metrics = TradingMetrics::new();
metrics.set_callback(Arc::new(move |sample| match sample {
    MetricSample::CounterInc { name, labels, value } if name == "trading_orders_total" => {
        orders_total.with_label_values(&[&labels["tool"], &labels["side"], &labels["status"]]).inc_by(value);
    }
    MetricSample::HistogramObserve { name, labels, value_secs } if name == "trading_tool_execute_duration_seconds" => {
        duration.with_label_values(&[&labels["tool"]]).observe(value_secs);
    }
    _ => {}
}));

// Expose to Prometheus
let encoder = prometheus::TextEncoder::new();
// Periodically write registry.gather() to HTTP response
```

### 3.2 Python + Prometheus (using `prometheus_client`)

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

# Start Prometheus exporter (separate HTTP port)
start_http_server(9100)

# Register callback
def on_sample(sample):
    kind, data = sample
    if kind == 'counter_inc' and data['name'] == 'trading_orders_total':
        orders_total.labels(**data['labels']).inc(data['value'])
    elif kind == 'histogram_observe' and data['name'] == 'trading_tool_execute_duration_seconds':
        duration.labels(**data['labels']).observe(data['value_secs'])

axon_quant.set_metrics_callback(on_sample)
```

### 3.3 Rust + OpenTelemetry (using `opentelemetry` crate)

```rust
use opentelemetry::metrics::MeterProvider;
let provider = opentelemetry_otlp::new_pipeline().install_simple();
let meter = provider.meter("axon-llm");

let orders_counter = meter.u64_counter("trading_orders_total").init();
let duration_hist = meter.f64_histogram("trading_tool_execute_duration_seconds").init();

metrics.set_callback(Arc::new(move |sample| match sample {
    MetricSample::CounterInc { name, labels, value } if name == "trading_orders_total" => {
        let attrs = labels.iter().map(|(k, v)| KeyValue::new(k, v)).collect::<Vec<_>>();
        orders_counter.add(value, &attrs);
    }
    // ...
    _ => {}
}));
```

## 4. Alert Recommendations (Application Configuration)

axon does not provide centralized alerting rules. Below are **alert recommendations** based on metrics for application reference:

| Alert Name | Trigger Condition | Severity | Action |
|-----------|------------------|----------|--------|
| HighRiskRejection | `rate(trading_risk_rejections_total[5m]) > 10` | warning | Check if LLM prompt has been jailbroken |
| CircuitBreakerOpen | `trading_risk_gate_blocked_total > 0` | critical | Immediate human takeover, check decision logs |
| BackendErrorSpike | `rate(trading_backend_errors_total[5m]) > 5` | critical | Check exchange API / OMS status |
| LatencyP99TooHigh | `histogram_quantile(0.99, rate(trading_tool_execute_duration_seconds_bucket[5m])) > 5` | warning | Check backend latency, network quality |
| DailyOrderBurst | `trading_daily_orders_count > 80% * max_daily_orders` | info | Approaching risk limit, prepare rate limiting |

## 5. Performance Overhead

`TradingMetrics` performance overhead is minimal:

- Counter increment: 1 atomic add (~10ns)
- Histogram observation: 1 atomic add + bucket lookup (~50ns)
- Callback overhead: 0 (without callback, internal only loads `Mutex<Option<...>>`)

Measured: Under 10K orders/sec stress test, metrics module CPU usage < 0.1%.

## Next Steps

- [Operations Runbook](operations-runbook.md) — Deployment, upgrade, troubleshooting
- [Architecture Overview](architecture.md) — System components
