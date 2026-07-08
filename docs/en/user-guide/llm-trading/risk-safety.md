# LLM Trading Risk & Safety

> Applicable version: axon-llm v0.3.0+
> Prerequisites: [overview.md](overview.md) §4

This document details axon-llm's three risk control defense lines + failure modes + recovery strategies. All risk control is **fail-closed** (any stage failure immediately rejects, never enters backend).

## 1. SafetyMode: DryRun / TwoPhase / Direct

### 1.1 Semantics

`SafetyMode` is the outermost interception before order placement:

| Mode | Behavior | Typical Use Case |
|------|----------|-----------------|
| `DryRun` | **Does not place real orders**, only tracing logs, returns `OrderAck { status: "DryRun" }` | LLM decision validation / integration testing |
| `TwoPhase` | **Two-phase confirmation**: First call returns `confirm_token` (uuid v4), second call with same token actually places order | High-risk operations requiring human in-the-loop approval |
| `Direct` | **Directly calls backend**, no interception | Production live trading (with other risk controls in place) |

### 1.2 TwoPhase Detailed Flow

```python
# First call
ack1 = place_order_tool.execute({
    "symbol": "BTC-USDT",
    "side": "Buy",
    "quantity": 0.1,
    "price": 50000.0,
    "confirm_token": None,
})
# ack1: { "status": "PendingConfirm", "confirm_token": "uuid-xxx", "order_id": None }

# Second call (must use same token)
ack2 = place_order_tool.execute({
    "symbol": "BTC-USDT",
    "side": "Buy",
    "quantity": 0.1,
    "price": 50000.0,
    "confirm_token": "uuid-xxx",
})
# ack2: { "status": "Filled", "order_id": "real-id", ... }
```

**Note**: `TwoPhase` state is in-memory (`PendingOrder` cache), token invalidates after restart, requiring first phase to be re-executed.

### 1.3 Selection Recommendations

- **Development / Integration / CI testing**: `DryRun`, never place orders
- **LLM agent canary deployment**: `TwoPhase`, human confirmation for critical operations
- **Production live trading (with multiple external audits)**: `Direct`

## 2. RiskLimits: Static Rules

`RiskLimits` is the second defense line before order placement, containing 4 static rules — any rule failure immediately rejects.

### 2.1 Rule List

| Rule | Field | Check Logic | Failure Message Example |
|------|-------|-------------|------------------------|
| Max order notional | `max_order_notional` | `quantity * price <= max_order_notional` | `"order notional 60000.0 exceeds limit 50000.0"` |
| Max daily orders | `max_daily_orders` | `daily_count < max_daily_orders` | `"daily order count 101 exceeds limit 100"` |
| Max position abs per symbol | `max_position_abs` | `|current_qty + side_delta| <= max_position_abs` | `"projected position 0.6 exceeds max abs 0.5"` |
| Allowed symbols whitelist | `allowed_symbols` | `symbol ∈ allowed_symbols` | `"symbol 'DOGE-USDT' not in allowed list"` |

### 2.2 Rule Combination Example

```python
risk = RiskLimits(
    max_order_notional=50_000.0,   # Single order ≤ 50k USDT
    max_daily_orders=100,            # Daily ≤ 100 orders
    max_position_abs=10.0,           # Single symbol ≤ 10 units
    allowed_symbols={"BTC-USDT", "ETH-USDT"},  # Only trade these two
)
```

### 2.3 `max_position_abs` Detailed Explanation

**Formula**: `projected = current_position + side_delta`, where:
- `current_position`: Current position queried from `backend.get_positions()`
- `side_delta`: `Buy` → `+quantity`, `Sell` → `-quantity`
- Failure condition: `|projected| > max_position_abs`

**Multi-scenario examples**:

```text
Initial: position = 0, max_abs = 0.5
├── Buy 0.3 -> projected = 0.3,  |0.3| = 0.3 ≤ 0.5 ✅ Pass
├── Buy 0.3 -> projected = 0.6,  |0.6| = 0.6 > 0.5 ❌ Reject
├── Sell 0.3 -> projected = 0,    |0.0| = 0.0 ≤ 0.5 ✅ Pass
└── Sell 0.8 -> projected = -0.8, |-0.8| = 0.8 > 0.5 ❌ Reject
```

**Note**: `max_position_abs` is **per-symbol isolated**, each symbol calculated independently. Allows `BTC-USDT` position 10 + `ETH-USDT` position 10, without affecting each other.

## 3. RiskGate: Dynamic Gates

`RiskGate` is the third defense line, handling "runtime states" (consecutive failures, intraday PnL threshold breaches, etc.).

### 3.1 Built-in Implementations

| Type | Trigger Logic | Dependencies |
|------|--------------|--------------|
| `AlwaysOpenGate` | Always allows (default) | None |
| `RejectionCircuitBreaker` | Opens after N consecutive risk rejections (auto-recovers after cooldown) | None (core lib built-in) |
| `RiskPnLCircuitBreaker` | Opens when daily PnL breaches threshold | `axon-risk` (feature = `trading-risk-extra`) |

### 3.2 `RejectionCircuitBreaker` Detailed

```rust
let gate = RejectionCircuitBreaker::new(
    threshold: 5,        // Open after 5 consecutive risk rejections
    cooldown_ms: 60_000,  // Cooldown 60 seconds
);
```

**State Machine**:

```text
        ┌──────────────────────────────────────┐
        ↓                                      │
    [Closed] ──N consecutive rejections──> [Open] ──cooldown ends──> [HalfOpen] ──one success──> [Closed]
        ↑                                          │
        └────────────────failure(back to Open)─────┘
```

- **Closed**: Normal passthrough
- **Open**: Rejects all orders, returns `CircuitBreakerOpen` error
- **HalfOpen**: Allows one trial order, success closes gate, failure re-enters Open

### 3.3 `RiskPnLCircuitBreaker` Detailed

```rust
let gate = RiskPnLCircuitBreaker::new(
    daily_pnl_floor: -1000.0,  // Opens when daily PnL drops below -1000 USDT
);
```

**Difference from `RejectionCircuitBreaker`**:

| Dimension | RejectionCircuitBreaker | RiskPnLCircuitBreaker |
|-----------|------------------------|-----------------------|
| Trigger Metric | Consecutive risk rejections | Daily PnL value |
| Use Case | Abnormal repeated rejections in LDM decisions | Real loss bottom protection |
| Dependencies | Zero (core lib) | `axon-risk` (feature gate) |
| Cooldown | Fixed time | Cross-day auto-reset (UTC 0:00) |

## 4. Failure Modes & Recovery

### 4.1 Failure Classification

| Failure Type | Failure Location | Recovery Strategy |
|-------------|-----------------|-------------------|
| `RiskLimitsViolation` | RiskLimits::check | Modify args, retry order |
| `CircuitBreakerOpen` | RiskGate | Wait cooldown / half-open trial success |
| `BackendError::Network` | Backend | Exponential backoff retry (application responsible) |
| `BackendError::Rejected` | Backend | Correct args, retry order |
| `BackendError::InsufficientFunds` | Backend | Reduce position before ordering |
| `BackendError::SymbolNotFound` | Backend | Check symbol spelling |

### 4.2 Unified Error Response Format

All tool failures return via `ToolError::ExecutionFailed(msg)`, where `msg` contains machine-readable prefix + human-readable description:

```json
{
  "error_type": "ExecutionFailed",
  "source": "RiskLimits",
  "message": "RiskLimits: order notional 60000.0 exceeds limit 50000.0"
}
```

```json
{
  "error_type": "ExecutionFailed",
  "source": "RiskGate",
  "message": "RiskGate: circuit breaker open (rejections=5, cooldown_remaining_ms=42137)"
}
```

LLM agents can decide to retry / ask user / change parameters based on the `source` field.

## 5. Security Best Practices

### 5.1 Enablement Order

1. **Must enable before production**: `RiskLimits` (basic rules)
2. **Strongly recommended**: `RejectionCircuitBreaker` (prevent LLM decision loops)
3. **Enable for high-sensitivity scenarios**: `TwoPhase` (human in-the-loop approval)
4. **Optional**: `RiskPnLCircuitBreaker` (requires `trading-risk-extra` feature)

### 5.2 Selection Matrix

| Scenario | SafetyMode | RiskLimits | RiskGate | TwoPhase |
|----------|-----------|-----------|----------|----------|
| Unit testing | DryRun | Disabled | AlwaysOpen | Off |
| Integration testing | DryRun | Disabled | AlwaysOpen | Off |
| Backtest evaluation | Direct | As needed | AlwaysOpen | Off |
| LLM agent canary | TwoPhase | Strict | RejectionCB | On |
| Production live | Direct | Strict | RejectionCB | As needed |
| High-sensitivity live | TwoPhase | Strict | RiskPnLCB | On |

### 5.3 Audit & Logging

All risk control decisions output tracing logs:

```text
INFO axon_llm::trading::tools::place_order: RiskLimits check passed order_id=ord-xxx
WARN axon_llm::trading::tools::place_order: RiskLimits rejected reason="notional exceeds" order_id=ord-yyy
ERROR axon_llm::trading::tools::place_order: RiskGate blocked reason="circuit breaker open" order_id=ord-zzz
```

Applications should integrate these logs into their own ELK / Loki / Datadog logging backends for compliance audit input.

## Next Steps

- [Metrics & Alerting](metrics-alerting.md) — Monitoring and alerting strategies
- [Operations Runbook](operations-runbook.md) — Troubleshooting
