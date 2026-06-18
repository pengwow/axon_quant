# LLM Trading Architecture

> Applicable version: axon-llm v0.1.0+
> Status: 🟢 In sync with code (Stage A~K fully delivered)

This document describes the trading pipeline architecture of `axon-llm`. Target audience: engineers integrating LLM agents into production trading systems.

## 1. System Components

The axon-llm trading pipeline consists of:

1. **LLM Agent Layer** — `ReActAgent` / `OpenAICompatBackend` etc., responsible for receiving user prompts, deciding which tools to call, and writing tool-returned JSON into LLM context.
2. **Tool Layer** — 4 trading tools: `PlaceOrderTool` / `QueryPortfolioTool` / `CancelOrderTool` / `ReplaceOrderTool`. Each tool implements the `Tool` trait (`async fn execute(&self, args: &str) -> Result<String, ToolError>`), accepting JSON string parameters and returning JSON string results.
3. **Risk Control Layer** — Three defense lines, chained within tools:
    - **SafetyMode** (`DryRun` / `TwoPhase` / `Direct`): Controls whether orders are actually placed
    - **RiskLimits** (`max_order_notional` / `max_daily_orders` / `max_position_abs` / `allowed_symbols`): Static risk control rules
    - **RiskGate** (`AlwaysOpenGate` / `RejectionCircuitBreaker` / `RiskPnLCircuitBreaker`): Dynamic risk control gates
4. **Backend Adapter Layer** — `TradingBackend` trait, 4 implementations:
    - `MockTradingBackend` (default, no feature flag, for testing)
    - `ExchangeTradingBackend` (feature = `trading-exchange`, connects to Binance/OKX)
    - `OmsTradingBackend` (feature = `trading-oms`, connects to axon-oms state machine)
    - `BacktestTradingBackend` (feature = `trading-backtest`, connects to axon-backtest L1/L2/L3)
5. **Monitoring Layer** — `TradingMetrics` (self-contained, no external monitoring stack dependency):
    - 5 `LabeledCounter` (order/cancel/modify/risk-reject/backend-fail)
    - 1 `LatencyHistogram` (end-to-end execute latency)
    - 1 gauge (daily order count mirrored from `DailyCounter`)
    - Two data outlets: `set_callback` real-time push / `snapshot` on-demand pull

## 2. Data Flow

```text
[User Prompt]
   ↓
[ReActAgent Decision]
   ↓ JSON args
[Tool::execute(args: &str)]
   ↓ parse args
[RiskLimits::check]────── ❌ -> ToolError::ExecutionFailed
   ↓
[RiskGate::is_blocked]─── ❌ -> ToolError::ExecutionFailed
   ↓
[TradingBackend::place_order / cancel_order / replace_order]
   ↓
[TradingError / OrderAck] -> JSON
   ↓
[ReActAgent writes JSON to context, continues LLM reasoning]
```

Key points:
- All tool args / results are **JSON strings**, most natural for LLM tool passthrough
- Risk control is fail-closed (any stage failure immediately rejects, does not enter backend)
- All backend calls are `async`, tools use `tokio::Runtime::block_on` to bridge internally
- Python side calls via PyO3 (see [Python Bindings](../../reference/python-bindings.md))

## 3. Backend Selection Decision Tree

```text
What do you want to do?
├── Unit tests / Integration tests / CI
│   └── MockTradingBackend (zero dependencies, default enabled)
├── Connect to real exchange (Binance / OKX testnet / mainnet)
│   └── ExchangeTradingBackend (feature = trading-exchange)
├── Connect to production order management system (axon-oms state machine)
│   └── OmsTradingBackend (feature = trading-oms)
├── Simulate LLM decisions on historical data (backtest-style evaluation)
│   └── BacktestTradingBackend (feature = trading-backtest)
└── Custom scenarios (internal matcher, paper trading, etc.)
    └── Implement TradingBackend trait
```

## 4. Security Model (Defense-in-Depth)

axon-llm's security model follows **defense-in-depth** principles — any single defense line failure still has the next layer:

| Level | Name | Trigger | Failure Behavior |
|-------|------|---------|-----------------|
| L0 | **Application prompt safety** | Caller | Entirely caller's responsibility (LLM jailbreak prevention, sensitive data masking) |
| L1 | **SafetyMode** | Tool entry | `DryRun` no order / `TwoPhase` two-phase confirmation / `Direct` passthrough |
| L2 | **RiskLimits** | Before order | Any rule failure immediately rejects (`fail-closed`) |
| L3 | **RiskGate** | Before order | Gate open allows, closed rejects (dynamic circuit breaker) |
| L4 | **TradingBackend** | Actual execution | Each backend implementation provides final defense (exchange built-in risk, OMS built-in risk) |

Detailed rules and failure modes see [Risk & Safety](risk-safety.md).

## 5. Monitoring Model (Lightweight + Pluggable)

axon-llm does **not** include any external monitoring stack (Prometheus exporter, Grafana dashboard, OpenTelemetry collector, etc.), only providing self-contained `TradingMetrics` collectors:

- Data outlet 1: **Callback** — `metrics.set_callback(|sample| { ... })`, real-time push to caller-registered sink
- Data outlet 2: **Snapshot** — `metrics.snapshot()` on-demand pull, returns `Vec<MetricSample>`

Application connects to their own team's monitoring backend:

- Rust applications: use `axum_prometheus` / `metrics-exporter-prometheus` / custom sink
- Python applications: use `prometheus_client` / `opentelemetry-sdk` / custom push

Application integration examples see [Metrics & Alerting](metrics-alerting.md) §3.

## 6. Core Crate Relationships

```text
axon-llm (trading submodule)
├── trading::tools  ──── PlaceOrderTool / QueryPortfolioTool / CancelOrderTool / ReplaceOrderTool
├── trading::risk   ──── SafetyMode / RiskLimits / RiskGate trait
├── trading::backend──── TradingBackend trait
├── trading::metrics──── LabeledCounter / LatencyHistogram / TradingMetrics
├── trading::circuit_breaker_gate
│   ├── RejectionCircuitBreaker (core lib, zero dependencies)
│   └── RiskPnLCircuitBreaker (feature = trading-risk-extra, wraps axon_risk::CircuitBreaker)
└── trading::python ───── PyO3 bindings (RiskLimits / MockTradingBackend / 4 tools / TradingMetrics)
```

## Next Steps

- [Risk & Safety](risk-safety.md) — Three defense lines detailed
- [Metrics & Alerting](metrics-alerting.md) — Monitoring data outlets
- [Operations Runbook](operations-runbook.md) — Deployment, upgrade, troubleshooting
