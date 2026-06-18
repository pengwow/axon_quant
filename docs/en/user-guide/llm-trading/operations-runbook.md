# LLM Trading Operations Runbook

> Applicable version: axon-llm v0.1.0+
> Audience: Operations / SRE / On-call Engineers
> Prerequisites: [overview.md](overview.md) [risk-safety.md](risk-safety.md) [metrics-alerting.md](metrics-alerting.md)

This document provides the complete runbook for deployment, upgrade, troubleshooting, and rollback of the LLM trading system. All commands assume the cargo workspace root directory.

axon-llm is a library with no standalone server process. This document assumes the application implements `bin/llm-trading-agent` main program in Rust (or Python via PyO3). This runbook only covers axon-llm related parts; main program details are determined by each team.

## 1. Deployment

### 1.1 System Requirements

- **Rust**: 1.96.0+ (enforced by `rust-toolchain.toml`)
- **Python**: 3.14.6 (default, pyenv managed; only for Python bindings)
- **CPU**: At least 4 cores
- **Memory**: Development 4GB / Production 8GB+
- **Network**: Outbound HTTPS (443) + WSS (9443) required for trading

### 1.2 Deployment Steps

```bash
# 1. Clone latest code
git clone https://github.com/pengwow/axon_quant.git /opt/axon_quant
cd /opt/axon_quant

# 2. Select release tag
git checkout v0.1.0  # or main / develop

# 3. Release build (LTO, 1 codegen-unit)
cargo build --workspace --release

# 4. (Optional) Install Python bindings
make python-install

# 5. Start application (determined by each team, this is just an example)
RUST_LOG=info /opt/axon_quant/target/release/llm-trading-agent \
  --config /etc/axon_quant/config.toml
```

### 1.3 Configuration Example (`/etc/axon_quant/config.toml`)

```toml
[trading]
backend = "exchange"  # mock | exchange | oms | backtest
api_key = "${AXON_API_KEY}"  # Read from environment variable
api_secret = "${AXON_API_SECRET}"
testnet = true  # Testnet

[safety]
mode = "TwoPhase"  # DryRun | TwoPhase | Direct

[risk]
max_order_notional = 50000.0
max_daily_orders = 100
max_position_abs = 10.0
allowed_symbols = ["BTC-USDT", "ETH-USDT"]

[gate]
type = "RejectionCircuitBreaker"
threshold = 5
cooldown_ms = 60000

[metrics]
# Application registers to its own monitoring backend
callback = "prometheus:http://localhost:9100/metrics"
```

## 2. Upgrade

### 2.1 Rolling Upgrade Steps

axon-llm is a library; upgrading affects `target/release/llm-trading-agent`. Standard rolling upgrade:

```bash
# 1. Fetch new code
cd /opt/axon_quant
git fetch --tags
git checkout v0.2.0  # Assume new version

# 2. Compile new binary (without interrupting old service)
cargo build --workspace --release

# 3. Health check new binary
./target/release/llm-trading-agent --version
./target/release/llm-trading-agent --self-check

# 4. Replace instance by instance (systemd / k8s / supervisor)
systemctl restart llm-trading-agent.service
# or
kubectl rollout restart deployment/llm-trading-agent

# 5. Observe new instance metrics / logs
journalctl -u llm-trading-agent -f
# or
kubectl logs -f deployment/llm-trading-agent
```

### 2.2 Database / State Compatibility

axon-llm has **no server-side persistent state** (all state is in-memory / in backend). Before upgrading, note:

- `RiskLimits::DailyCounter`: In-memory, resets on restart. If application needs persistence, mirror to external storage (Redis / DB)
- `RejectionCircuitBreaker`: In-memory, resets on restart
- `TradingMetrics`: In-memory, resets on restart
- `TwoPhase::PendingOrder`: In-memory, token invalidates on restart (impact see [Risk & Safety](risk-safety.md) §1.2)

**Important**: Stateless upgrades are safe, no migration scripts needed.

### 2.3 Breaking Change Check

Before upgrading, must read CHANGELOG's **BREAKING CHANGES** section. Typical breaking changes:

- `TradingBackend` trait new required methods (though designed with default implementations + backward compatibility)
- `RiskLimits` new required fields
- `Tool` trait signature changes

**Application adaptation process**: First run E2E test suite in staging `cargo test -p axon-llm --test llm_trading_*_e2e`, then upgrade production after passing.

## 3. Troubleshooting

### 3.1 LLM Agent All Orders Failing

**Symptom**: `trading_risk_rejections_total` continuously increasing, `trading_orders_total{status="success"}` = 0.

**Troubleshooting Steps**:

1. **Check SafetyMode**:
   ```bash
   journalctl -u llm-trading-agent | grep -i "DryRun\|TwoPhase\|Direct"
   ```
   If DryRun / TwoPhase, confirm SafetyMode configuration is correct.

2. **Check RiskLimits**:
   ```bash
   journalctl -u llm-trading-agent | grep "RiskLimits"
   ```
   If seeing `"order notional X exceeds limit Y"` messages, LLM decisions exceed risk rules.
   Action: Widen `max_order_notional` / `max_daily_orders` / `max_position_abs`, or constrain LLM prompt.

3. **Check RiskGate**:
   ```bash
   journalctl -u llm-trading-agent | grep "RiskGate"
   ```
   If seeing `"circuit breaker open"`, consecutive rejections reached threshold, gate is open.
   Action: Wait for cooldown (default 60s), or manually call `gate.reset()` (application implements).

4. **Check Backend**:
   ```bash
   journalctl -u llm-trading-agent | grep "BackendError"
   ```
   If seeing network errors / rejections / timeouts, continue to §3.2.

### 3.2 Backend Call Failures

**Symptom**: `trading_backend_errors_total` continuously increasing.

**Troubleshooting Steps**:

1. **Confirm backend type**:
   - `exchange`: Check exchange API status (https://www.binance.com/en/support/announcement)
   - `oms`: Check axon-oms service status / port
   - `backtest`: No network issues, check `MarketData` data integrity
   - `mock`: No network issues, check mock state

2. **Exchange backend**:
   ```bash
   # Test connectivity
   curl -v https://api.binance.com/api/v3/ping

   # Test signing
   PYO3_PYTHON=.venv/bin/python python -c "
   import axon_quant
   b = axon_quant.ExchangeTradingBackend(
       api_key='xxx', api_secret='yyy', testnet=True, exchange='binance'
   )
   print(b.get_balances())
   "
   ```

3. **OMS backend**:
   ```bash
   # Check OMS process
   systemctl status axon-oms
   # Check port
   ss -lnt | grep 8080
   # Health check
   curl http://localhost:8080/healthz
   ```

4. **Backtest backend**:
   - Check if `MarketData` has valid data
   - Check if time range covers strategy's required period

### 3.3 Performance Degradation

**Symptom**: `trading_tool_execute_duration_seconds` P99 > 1s.

**Troubleshooting Steps**:

1. **Determine if LLM call or backend call is slow**:
   - Breakdown: `trading_tool_execute_duration_seconds` includes LLM decision time + risk check + backend
   - Add extra histograms in application code: `llm_decision_duration_seconds`, `risk_check_duration_seconds`, `backend_call_duration_seconds`

2. **Backend slow**:
   - Exchange: Check exchange API latency
   - OMS: Check OMS service health
   - Backtest: Check `L1MatchingEngine` matching performance (`cargo bench -p axon-backtest`)

3. **Risk control overhead**:
   - Pure `RiskLimits::check` takes < 1μs, should not be bottleneck
   - `RejectionCircuitBreaker` takes < 100ns, should not be bottleneck

### 3.4 Process Crash

**Symptom**: `llm-trading-agent` process exits, systemd / k8s continuously restarts.

**Troubleshooting Steps**:

1. **View panic information**:
   ```bash
   journalctl -u llm-trading-agent -n 200 | grep -A 20 "panic\|FATAL"
   ```

2. **Common panic causes**:
   - `unwrap()` on uninitialized field in `MockTradingBackend`: Check if `setup()` is called
   - `tokio::Runtime` nesting: Check if `block_on` is called in thread with running runtime
   - Serialization errors: Check `serde_json` deserialization parameters

3. **Core dump**:
   ```bash
   ulimit -c unlimited
   echo "/tmp/core.%e.%p" > /proc/sys/kernel/core_pattern
   # Restart process, trigger panic, obtain core
   gdb /opt/axon_quant/target/release/llm-trading-agent /tmp/core.llm-trading-agent.1234
   ```

## 4. Rollback

### 4.1 Quick Rollback

```bash
cd /opt/axon_quant

# 1. Switch back to old tag
git checkout v0.1.0

# 2. Recompile
cargo build --workspace --release

# 3. Restart service
systemctl restart llm-trading-agent

# 4. Observe old version metrics
journalctl -u llm-trading-agent -f
```

### 4.2 Canary Rollback (k8s)

```bash
# Rollback to previous deployment version
kubectl rollout undo deployment/llm-trading-agent

# Pause rollback (if issues persist after rollback)
kubectl rollout pause deployment/llm-trading-agent
```

## 5. Daily Operations Checklist

### 5.1 Daily

- [ ] Check `trading_orders_total` trend is normal
- [ ] Check `trading_risk_rejections_total` for spikes
- [ ] Check `trading_backend_errors_total` for spikes
- [ ] Check `trading_daily_orders_count` approaching `max_daily_orders`
- [ ] Check LLM agent logs for anomalies (panic / unwrap / timeout)

### 5.2 Weekly

- [ ] Review this week's LLM decision samples (sample from `RiskLimits` rejected samples)
- [ ] Check if axon-llm has new version release (CHANGELOG)
- [ ] Check if exchange API has breaking change announcements
- [ ] Check `RejectionCircuitBreaker` trigger frequency, evaluate if threshold needs adjustment

### 5.3 Monthly

- [ ] Run regression test suite `cargo test --workspace`
- [ ] Run LLM integration E2E tests `cargo test -p axon-llm --test llm_trading_*_e2e`
- [ ] Check axon-llm release notes, plan upgrade
- [ ] Review this month's PnL, evaluate if RiskLimits thresholds are reasonable

## 6. Emergency Contacts

| Role | Contact |
|------|---------|
| axon-llm maintainer | (GitHub Issues / Team Slack) |
| Exchange integration | (Official exchange support) |
| OMS maintainer | (Internal team) |
| LLM provider outage | OpenAI / Anthropic / Other official status page |

## Next Steps

- [Architecture Overview](architecture.md) — System components
- [Risk & Safety](risk-safety.md) — Three defense lines
- [Metrics & Alerting](metrics-alerting.md) — Monitoring data
