# `axon-llm::swarm` Multi-Agent Orchestration (0.3.0 P0 Workflow B Wrap-up)

> Applies to: `axon-llm` v0.2.0+
> Status: **Implemented** (Workflow B Batch 1-4 fully wrapped up)
> Plan: `.axon-internal/plans/2026-07-06-v0.3.0-p0-implementation.md` §3

`SwarmOrchestrator` chains 4 agents (Market / Risk / Execution / Audit) into a runnable pipeline,
integrated with `HarnessBridge` for final adjudication, and `TradingBackend` for real order
submission to `MockTradingBackend` or live exchanges. Python side can construct agents, start
the loop, inject signals, and read stats directly.

## 1. 4-Agent Architecture

```text
                 ┌────────────────────────────┐
                 │     SwarmOrchestrator       │
                 │  ┌──────────────────────┐   │
                 │  │   run_loop_arc       │   │◀──── inject_market_signal
                 │  │   dispatch()         │   │      inject_vote_response
                 │  └──────────────────────┘   │
                 │  ConsensusManager + Stats   │
                 └─────┬──────┬──────┬─────────┘
                       │      │      │      (mpsc inbox)
        ┌──────────────┘      │      └────────────┐
        │                     │                    │
   ┌────▼────┐         ┌─────▼─────┐         ┌────▼────┐
   │ Market  │  ───▶   │   Risk    │  ───▶   │Execution│  ──▶  PlaceOrderTool
   │ Agent   │ Market  │  Agent    │  ───▶   │ Agent   │      QueryPortfolioTool
   │         │ Signal  │           │  Risk   │         │      TradingBackend
   └─────────┘         └───────────┘  Assess└─────────┘
        │                                          │
        │                                          ▼
        │                                    ┌──────────┐
        │                                    │  Audit   │
        │                                    │  Agent   │
        │                                    └──────────┘
        ▼
   MarketDataSource (Mock / WS / CSV)
```

**Key design points**:
- Each agent holds `Box<dyn DeclarativeAgentRunner>`, reusing DeclarativeAgent abstraction
- Inter-agent communication via `tokio::mpsc` (no shared state)
- Orchestrator main loop listens to inbox, routes by `MessageContent`
- `HarnessBridge` held by SwarmOrchestrator + shared with each agent as `Arc`

## 2. Message Routing Table (inside `run_loop`)

| Incoming Message | Action |
|---|---|
| `MarketAnalysis(signal)` | Create `TradeDecision` vote, broadcast `VoteRequest` to Risk + Execution |
| `RiskAssessment{approved=true}` | Forward to Execution agent for `ExecutionRequest` |
| `RiskAssessment{approved=false}` | Broadcast to Audit for rejection record |
| `ExecutionResult(result)` | Broadcast to Audit |
| `VoteResult{passed=true}` | Call `HarnessBridge.adjudicate()` for final decision |
| `VoteResponse(response)` | Cast into `ConsensusManager`, emit `VoteResult` on quorum |
| `Shutdown` | Set `shutdown_requested=true`, exit main loop |
| `Heartbeat` / others | Ignored |

### Voting Consensus + Harness Integration

```text
VoteResponse → ConsensusManager
                    │
                    ▼ SimpleMajority (≥2 votes)
              VoteResult{passed=true}
                    │
                    ▼
        HarnessBridge.adjudicate(intent, ctx)
                    │
       ┌────────────┼────────────┬────────────┐
       ▼            ▼            ▼            ▼
   Approved     Rejected   CircuitBreak  NeedRevision
   (execute)   (audit)    (shutdown)      (re-analyze)
```

When no `HarnessBridge` is configured, fallback is `Adjudication::Approved` (zero-intrusion mode:
vote passing ⇒ immediate approval).

## 3. Key Modules

### 3.1 `DeclarativeAgentRunner` trait

```rust
#[async_trait]
pub trait DeclarativeAgentRunner: Send + Sync {
    fn id(&self) -> &AgentId;
    fn role(&self) -> AgentRole;
    fn status(&self) -> AgentStatus;
    async fn handle_message(&mut self, msg: AgentMessage) -> Result<RunnerOutput, SwarmError>;
}
```

- **Object safety**: satisfies object-safety, allows `Arc<dyn DeclarativeAgentRunner>` across tasks
- **Sync bound**: `Status` returns `Copy`, `handle_message` takes `&mut self` + async, allows concurrency

### 3.2 Four Agents

| Agent | Input | Output | Config |
|---|---|---|---|
| `MarketAgent` | `MarketDataSource` (tick) | `MarketSignal` | `symbols` + `price_change_threshold` |
| `RiskAgent` | `MarketSignal` | `RiskAssessment` | `RiskAgentConfig` (default thresholds) |
| `ExecutionAgent` | `RiskAssessment{approved=true}` | `ExecutionResult` (via `PlaceOrderTool`) | `TradingTools { place_order, query_portfolio }` |
| `AuditAgent` | `ExecutionResult` | (audit record) | `AuditAgentConfig` (default) |

### 3.3 `PaperTradingBackend`

`PaperTradingBackend` implements the Stage K `TradingBackend` trait, simulating real trading:

- **Slippage**: `slippage_bps` (basis points, price up for buy / down for sell)
- **Commission**: `commission_bps` (on notional)
- **State**: `cash` + `positions: HashMap<symbol, (qty, entry_price)>` + `last_prices`
- **Price update**: `place_order` updates `last_prices[symbol] = fill_price`
- **Cash flow**: Buy deducts `notional * (1 + commission_bps)`, Sell adds `notional * (1 - commission_bps)`

`get_balance()` returns `cash + Σ(qty * last_price)` (real-time NAV),
`get_positions()` returns `(symbol, qty, entry_price, current_price, unrealized_pnl)`.

### 3.4 `SwarmOrchestrator::run_loop_arc`

`Arc<TokioMutex<SwarmOrchestrator>>` shared across owners. Main loop:

```rust
pub async fn run_loop_arc(
    orchestrator: Arc<TokioMutex<Self>>,
    mut inbox_rx: mpsc::Receiver<AgentMessage>,
) {
    let tick = Duration::from_millis(guard.config.loop_tick_ms);
    loop {
        if guard.shutdown_requested { break; }
        let next = timeout(tick, inbox_rx.recv()).await;
        match next {
            Ok(Some(msg)) => dispatch(msg).await,
            Ok(None) => break,    // channel closed
            Err(_) => continue,   // timeout, recheck shutdown
        }
    }
}
```

**Exit conditions**: Shutdown message / `request_shutdown()` / inbox channel closed (all agent outboxes dropped).

## 4. Python Integration

### 4.1 Typical Usage

```python
from axon_quant.llm import (
    SwarmConfig, SwarmOrchestrator, MarketSignal, SignalType,
    TradingTools,
)
from axon_quant.trading import (
    MockTradingBackend, PlaceOrderTool, QueryPortfolioTool, RiskLimits,
)

# 1. Construct orchestrator
config = SwarmConfig(vote_timeout_ms=5000, loop_tick_ms=100)
orch = SwarmOrchestrator(config)

# 2. Register 4 agents (register_*_agent auto-starts run_loop)
orch.register_market_agent(agent_id="m0", symbols=["BTC-USDT"])
orch.register_risk_agent(agent_id="r0")

# ExecutionAgent must receive tools (otherwise mock mode)
backend = MockTradingBackend()
risk = RiskLimits(allowed_symbols=["BTC-USDT"])
place = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk)
query = QueryPortfolioTool(backend=backend)
tools = TradingTools(place_order=place, query_portfolio=query)
orch.register_execution_agent(agent_id="e0", tools=tools)

orch.register_audit_agent(agent_id="a0")

# 3. Inject MarketSignal → orchestrator triggers vote + forward
orch.inject_market_signal(MarketSignal(
    symbol="BTC-USDT",
    signal_type=SignalType.Buy,
    confidence=0.9,
    reasoning="momentum breakout",
))

# 4. Read stats
import time; time.sleep(0.5)
stats = orch.stats()
print(stats["market_signals"], stats["votes_created"])

# 5. Shutdown
orch.stop()
```

### 4.2 Full Pipeline Demo

See [`examples/18_harness/swarm_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/18_harness/swarm_demo.py).

## 5. Test Coverage

| Test File | Count | Content |
|---|---|---|
| `python/tests/test_swarm_pipeline_e2e.py` | **25/25 ✅** | Enums/structs + 4 agent register + lifecycle + inject + stats |
| `crates/axon-llm/src/swarm/` lib unittests | all pass | DeclarativeAgentRunner / Orchestrator / Vote / 4 agents / market_data |
| `crates/axon-llm/src/trading/paper_backend.rs` lib unittests | all pass | place_order / balance / position with slippage + commission |

**Total**: 322 lib unittests + 74 integration + 3 doctests + 25 Python E2E = **424 all pass**.

## 6. Known Limitations (deferred to 0.3.x)

- `PlaceOrderTool` defaults to `dry_run` mode; `direct` / `two_phase` modes need external two-step confirmation
- `RiskAgent` defaults to `approved=true` (happy path); real risk logic (volatility / VaR / limits) deferred to 0.3.x
- Cross-process coordination (distributed swarm) deferred to 0.4.0
- Live exchange integration (`axon-llm::trading::exchange` 8 `unimplemented!()` sites) deferred to 0.3.x
