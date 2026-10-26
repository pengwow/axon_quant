# Architecture Overview

> Applicable version: AXON v0.21.0+ (unified trading kernel)

## System Overview

AXON introduced the `axon-kernel` unified trading kernel in **v0.21.0**, collapsing the backtest / paper / live paths onto a single `Kernel` main loop (`kernel_loop.rs` → `BarLifecycle::on_bar_closed` nine-step per-bar orchestration). Mode differences are **assembly-only**: data pump (historical `build_frames` producing `GridFrame` vs. real-time `KernelPump`) and `ExecutionSink` (`BacktestSink` matching / `PaperSink` simulated / `LiveSink` live with `ExchangeGateway`). The main loop itself has no mode branches.

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│                         AXON Architecture (v0.22.1)                         │
├──────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌───────────────────────────── Python Drivers ──────────────────────────────┐│
│  │  LiveDriver / PaperDriver  │  ShadowDiff  │  LiveRecovery (journal resume)││
│  └────────────┬───────────────┴──────┬───────┴───────────┬─────────────────┘│
│               │                       │                   │                  │
│  ┌────────────▼───────────────────────▼───────────────────▼─────────────────┐│
│  │                          axon-worker (strategy orchestration)            ││
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐ ││
│  │  │ PyKernel       │  │ BacktestRunner│  │ Reconcile    │  │ Exchange     │ ││
│  │  │ Python host    │  │ legacy fallback│  │ 3-way reconcile│  │ Gateway      │ ││
│  │  └──────┬───────┘  └──────┬───────┘  └──────────────┘  └──────┬───────┘ ││
│  └─────────┼─────────────────┼────────────────────────────────────┼─────────┘│
│            │                 │                                    │           │
│  ┌─────────▼─────────────────▼────────────────────────────────────▼─────────┐│
│  │                        axon-kernel (unified trading kernel)              ││
│  │  Kernel.run_history / Kernel.step / Kernel.run_realtime                  ││
│  │  BarLifecycle::on_bar_closed  nine-step per-bar orchestration            ││
│  │  ┌─────────┐ ┌──────────┐ ┌────────────────┐ ┌───────────────────────┐   ││
│  │  │EventQueue│ │ Journal  │ │KernelCore      │ │BarGridBarrier         │   ││
│  │  └─────────┘ └──────────┘ └────────────────┘ └───────────────────────┘   ││
│  │  ExecutionSink trait ──► BacktestSink / PaperSink / LiveSink             ││
│  │  Pumps: build_frames (historical) │ KernelPump (real-time gap backfill)   ││
│  └──────────────────────────────────────────────────────────────────────────┘│
│               │                         │                         │           │
│  ┌────────────▼──────┐  ┌──────────────▼──────┐  ┌──────────────▼──────┐    │
│  │  axon-backtest    │  │    axon-exchange     │  │      axon-oms       │    │
│  │  BacktestSink     │  │  ExchangeGateway    │  │ LiveSink order FSM  │    │
│  │  matching + books │  │  WS market + user    │  │ VenueUpdate bridge  │    │
│  │  isolated margin  │  │  REST submit         │  │                    │    │
│  └─────────┬─────────┘  └──────────────────────┘  └─────────────────────┘    │
│            │                                                                   │
│  ┌─────────▼────────────────────────────────────────────────────────────────┐ │
│  │                    axon-core (shared types / contracts / ledger prims)   │ │
│  └──────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ┌─────────────────────── Training pipeline (decoupled from kernel) ───────┐ │
│  │  axon-hpo │ axon-rl │ axon-walk-forward │ axon-tracker │ axon-registry │ │
│  └──────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

**Dependency directions (verified from `Cargo.toml`)**:

```text
axon-core ◄─────── axon-backtest ◄─────── axon-kernel ◄────────────┐
     ▲                    ▲                    ▲                    │
     │                    │                    │                    ▼
     └────────────────────┴───────────────── axon-worker ────── axon-exchange
                                             │
                                             ▼
                                          axon-oms
```

- `axon-kernel → axon-backtest`: `BacktestSink` reuses `axon-backtest`'s matching + ledger (including the v0.21.0 isolated margin engine).
- `axon-worker → axon-kernel + axon-exchange + axon-backtest + axon-oms`: strategy orchestration layer holding `LiveDriver / PaperDriver / PyKernel`, wiring external market data into the kernel and routing kernel order intents to `LiveSink`. The legacy `BacktestRunner` is preserved as a backtest fallback.
- `axon-exchange → axon-core`: exchange adapters live in their own crate; the kernel is agnostic to Binance vs. OKX.

## Data Flow

### Backtesting Data Flow

Historical K-lines / matching data are collapsed into `GridFrame`s by `build_frames` (time-aligned across instruments) and fed directly into `Kernel.run_history`, executing the **exact same** `BarLifecycle` nine-step orchestration as live trading.

```text
Historical data (CSV / Arrow / Parquet)
    │
    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  build_frames ──► GridFrame ──► Kernel.load_frames ──► Kernel.step loop  │
│  (axon-kernel::pump)                                                    │
└────────────────────────────────────┬────────────────────────────────────┘
                                     │
                                     ▼
                 ┌─────────────────────────────────────┐
                 │  BarLifecycle::on_bar_closed        │
                 │  nine-step per-bar orchestration    │
                 │                                     │
                 │  ① clock align + forward-fill prices│
                 │  ② liquidation snapshot             │
                 │  ③ seed liquidity + auto rebalance   │
                 │     + funding settle + sample_bar_nav│
                 │  ④ per-BarClosed per-Host decision   │
                 │  ⑤ portfolio budget constraint      │
                 │  ⑥ sink.finish_bar final commit      │
                 │  ⑦ funding fee settle               │
                 │  ⑧ margin check + liquidation        │
                 │  ⑨ sample_bar_nav (covered in ③)    │
                 └──────────────┬──────────────────────┘
                                │
                                ▼
                  ┌────────────────────────────┐
                  │  BacktestSink               │
                  │  (axon-backtest matcher)   │
                  │  L1 / L2 / L3 matching     │
                  │  latency / fee / market     │
                  │  impact models              │
                  │  isolated margin (v0.21.0+) │
                  └──────────────┬─────────────┘
                                 │
                                 ▼
                  ┌────────────────────────────┐
                  │  KernelResult (aggregated) │
                  │  ├ RunResult                │
                  │  │   (PnL / fills / NAV /   │
                  │  │    drawdown / positions) │
                  │  ├ duration (wall clock)    │
                  │  ├ bars_processed           │
                  │  └ orders_submitted         │
                  └────────────────────────────┘
                                 │
                                 ▼
                  ┌────────────────────────────┐
                  │  BacktestResult             │
                  │  (axon-worker Python API)   │
                  │  legacy BacktestRunner      │
                  │  degraded to fallback       │
                  └────────────────────────────┘
```

### Live Trading Data Flow (live / paper share the same main loop)

The live path diverges **only in the pump + sink assembly**: `ExchangeGateway` feeds WS market + user-data streams into `KernelPump` (with automatic gap backfill on reconnect), which drives the main loop via `Kernel.step`. Execution goes through `LiveSink` (inline `RiskCheck` + order submission + `VenueUpdate` conversion) or `PaperSink` (simulated matching, no outbound network).

```text
┌──────────────────────────── live side (axon-worker) ─────────────────────────────┐
│                                                                                   │
│  ExchangeGateway                                                                  │
│  ┌─────────────────┐   ┌──────────────────┐   ┌──────────────────┐              │
│  │ Market WS (mark / │   │ user-data stream  │   │ REST submit        │              │
│  │ kline / book)     │   │ (orders / fills)  │   │                    │              │
│  └────────┬────────┘   └────────┬─────────┘   └────────▲─────────┘              │
│           │                      │                      │                          │
│           ▼                      ▼                      │                          │
│  ┌──────────────────────────────────────┐               │                          │
│  │         KernelPump                   │               │                          │
│  │  gap backfill ──► GridFrame          │               │                          │
│  │  multi-instrument aligned to a single │               │                          │
│  │  grid timestamp                      │               │                          │
│  └──────────────────────┬───────────────┘               │                          │
│                         │                               │                          │
└─────────────────────────┼───────────────────────────────┼──────────────────────────┘
                          ▼                               │
┌─────────────────────────────────────────────────────────┼──────────────────────────┐
│                              axon-kernel main loop (identical to backtest)         │
│                                                                                    │
│  Kernel.step (poll)                                                                │
│       │                                                                            │
│       ▼                                                                            │
│  ┌─────────────────────────────────────┐                                          │
│  │  BarLifecycle::on_bar_closed        │   nine-step per-bar (same as backtest)   │
│  └──────────────┬──────────────────────┘                                          │
│                 │                                                                  │
│                 ▼                                                                  │
│  ┌──────────────────────────────────────────────────────────────────┐              │
│  │  ExecutionSink                                                  │              │
│  │                                                                 │              │
│  │  LiveSink (production)                                          │              │
│  │  ├─ Inline RiskCheck (rejects at intent level)                  │              │
│  │  ├─ Order intent ──► axon-oms order FSM                         │              │
│  │  ├─ VenueUpdate conversion: exchange fills → internal events     │              │
│  │  └─ 3-way reconcile (price / equity / order book)               │              │
│  │                                                                 │              │
│  │  PaperSink (simulation)                                         │              │
│  │  └─ Same chain as LiveSink, but no outbound network             │              │
│  │     (internal matching + slippage simulation)                   │              │
│  └──────────────────────────────┬─────────────────────────────────┘              │
└─────────────────────────────────┼──────────────────────────────────────────────────┘
                                  ▼
┌─────────────────────────────── Post-trade ────────────────────────────────────────────────────┐
│                                                                                                 │
│  ┌──────────────────────────────────────┐                                                     │
│  │  Journal (event sourcing)             │                                                     │
│  │  Journal::create / append / resume   │                                                     │
│  │  Kernel.attach_journal(path)         │                                                     │
│  │  LiveDriver.start(journal_path)      │                                                     │
│  │  LiveRecovery: replay journal         │                                                     │
│  │  ──► Kernel resumes at checkpoint    │                                                     │
│  └──────────────────────────────────────┘                                                     │
│                                                                                                 │
│  ┌──────────────────────────────────────┐                                                     │
│  │  3-way reconcile                      │                                                     │
│  │  latest price / equity / order book   │                                                     │
│  └──────────────────────────────────────┘                                                     │
│                                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────────────────────┘
```

**Backtest vs. live consensus**:
- ✅ Shared: `Kernel`, `BarLifecycle::on_bar_closed` nine-step orchestration, ledger core, Host decisions, event routing, journal (backtest can also journal and replay)
- ❌ Not shared: data pump (`build_frames` historical vs. `KernelPump` real-time) and `ExecutionSink` (`BacktestSink` matcher vs. `LiveSink` / `PaperSink`)
- ❌ Live-only: `ExchangeGateway` WS market, user-data stream, gap backfill, 3-way reconcile, crash recovery, REST order submission

### Training Pipeline Data Flow

The training pipeline (`axon-hpo / axon-rl / axon-walk-forward / axon-tracker / axon-registry`) is decoupled from the kernelization and keeps its closed-loop structure.

```text
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  axon-hpo   │────▶│  axon-rl    │────▶│  axon-      │
│  Hyperparam │     │  RL env     │     │  walk-      │
│  search     │     │             │     │  forward    │
└─────────────┘     └─────────────┘     └──────┬──────┘
                                               │
                                               ▼
                                        ┌─────────────┐
                                        │  axon-      │
                                        │  tracker    │
                                        │  experiment │
                                        │  tracking   │
                                        └──────┬──────┘
                                               │
                                               ▼
                                        ┌─────────────┐
                                        │  axon-      │
                                        │  registry   │
                                        │  model reg  │
                                        └─────────────┘
```

## Module Dependencies

```text
axon-core ◄──────────────────────────────────────────┐
    │                                                 │
    ├──► axon-backtest                                │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-kernel                                  │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-worker                                  │
    │        │                                        │
    │        ├──► axon-exchange ◄─────────────────────┤
    │        │         │                              │
    │        │         └──► axon-oms ◄───────────────┤
    │        │                                        │
    │        ▼                                        │
    ├──► axon-rl                                      │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-hpo                                     │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-walk-forward                            │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-tracker                                 │
    │        │                                        │
    │        ▼                                        │
    ├──► axon-registry                                │
    │                                                 │
    ├──► axon-risk                                    │
    │                                                 │
    ├──► axon-inference                               │
    │                                                 │
    └──► axon-monitor ◄───────────────────────────────┘
```

**Key edges (verified from `Cargo.toml`)**:
- `axon-kernel ──► axon-backtest`: `BacktestSink` reuses `axon-backtest`'s matching + ledger.
- `axon-worker ──► axon-kernel`: strategy orchestration layer holds `PyKernel`, drives `Kernel.run_history` / `Kernel.step`.
- `axon-worker ──► axon-exchange`: data source for `ExchangeGateway` and the live `KernelPump`.
- `axon-worker ──► axon-oms`: order FSM and `VenueUpdate` conversion used by `LiveSink`.

## Event System

### Event Types

```text
Event
├── MarketDataEvent
│   ├── Tick        (Trade tick)
│   ├── Bar         (K-line)
│   └── OrderBook   (Order book snapshot)
├── OrderEvent
│   ├── Submitted   (Order submitted)
│   ├── Cancelled   (Order cancelled)
│   ├── Modified    (Order modified)
│   └── Rejected    (Order rejected)
├── FillEvent
│   └── Trade       (Trade record)
└── SystemEvent
    ├── Heartbeat   (Heartbeat)
    ├── SessionStart(Session start)
    ├── SessionEnd  (Session end)
    └── Error       (Error)
```

### Event Routing

`Kernel` drives the main loop via `EventQueue` (binary heap) ordered by timestamp; `BarLifecycle::on_bar_closed` dispatches the nine-step orchestration per frame. There is no separate router for live vs. backtest.

```text
EventQueue (BinaryHeap)
    │
    ▼
Kernel
    ├── BarLifecycle::on_bar_closed
    │     └──► StrategyHost.on_event (per-BarClosed)
    │           └──► ExecutionSink.submit(OrderIntent)
    │                 ├──► BacktestSink  (matching → FillEvent)
    │                 ├──► LiveSink      (RiskCheck → axon-oms → exchange)
    │                 └──► PaperSink     (simulated matching)
    ├── axon-risk (inline in LiveSink, not a separate loop)
    └── axon-monitor (metrics / alerts)
```

## Concurrency Model

```text
┌─────────────────────────────────────────────────────────┐
│           Kernel main loop (single-threaded,            │
│                     tokio current-thread)                │
│                                                         │
│  ┌─────────────┐     ┌─────────────┐     ┌───────────┐ │
│  │ EventQueue  │────▶│ BarLifecycle│────▶│ Execution │ │
│  │ (BinaryHeap) │     │ nine-step   │     │ Sink      │ │
│  └─────────────┘     └─────────────┘     └───────────┘ │
└─────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────┐
│                    I/O thread pool (tokio)               │
│                                                         │
│  ┌─────────────┐     ┌─────────────┐     ┌───────────┐ │
│  │ Exchange     │     │ REST API    │     │ File watch│ │
│  │ Gateway WS   │     │ (submit)    │     │ (notify)  │ │
│  └─────────────┘     └─────────────┘     └───────────┘ │
│                                                         │
│  KernelPump runs on the I/O pool, pushing GridFrames    │
│  into the kernel via a sync channel.                    │
└─────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────┐
│                    Compute thread pool (rayon)            │
│                                                         │
│  ┌─────────────┐     ┌─────────────┐     ┌───────────┐ │
│  │ Batch       │     │ VaR         │     │ Data      │ │
│  │ inference   │     │ computation │     │ processing│ │
│  │ (parallel)  │     │ (parallel)  │     │ (parallel)│ │
│  └─────────────┘     └─────────────┘     └───────────┘ │
└─────────────────────────────────────────────────────────┘
```

## Performance Critical Paths

### Risk Check (12ns)

Executed inline in `LiveSink`, no longer a pre-step in a separate loop.

```text
check_order
    │
    ├─→ circuit-breaker check (AtomicBool, ~5ns)
    │       └─→ short-circuit when inactive
    │
    ├─→ order size check (~10ns)
    │       └─→ limit_price() × quantity
    │
    ├─→ position limit check (~50ns)
    │       └─→ HashMap lookup
    │
    ├─→ leverage check (~20ns)
    │       └─→ nav / base_cash
    │
    └─→ drawdown check (~20ns)
            └─→ (peak - current) / peak
```

### Order Submission (1.2µs)

```text
submit
    │
    ├─→ idempotency check (HashMap lookup)
    │
    ├─→ UUID v7 generation
    │
    ├─→ state update (New → Submitted)
    │
    ├─→ active-order insert (HashMap)
    │
    └─→ history append (Vec)
```

## Data Storage

### Arrow/Parquet Columnar Storage

```text
┌─────────────────────────────────────────┐
│              Parquet file               │
├─────────────────────────────────────────┤
│  timestamp │ price │ quantity │ side    │
│  (i64)     │ (f64) │ (f64)    │ (u8)   │
├─────────────────────────────────────────┤
│  1000 rows / batch                      │
│  Compression: Snappy / ZSTD             │
│  Read/write latency: < 15ms (1M ticks)  │
└─────────────────────────────────────────┘
```

### Journal (Event Sourcing)

```text
Journal file (line-delimited JSON)
├── First line: manifest (kernel fingerprint: config / format version / run_id)
├── Every other line: JournalEntry
│   ├── seq: u64              monotonic index
│   ├── ev: Event             incoming event (market / exchange fills)
│   └── state: Option<Json>   per-bar state at this frame
├── Kernel.attach_journal(path)          truncate + write manifest
├── Kernel.enqueue_external(ev, state)   auto append (suppressed during replay)
├── Journal::resume(path, manifest)       crash-recovery entry point
└── LiveRecovery: replay journal → Kernel resumes at checkpoint
```

### Memory Layout

```text
Tick (#[repr(C)], 32 bytes)
├── timestamp: i64    (8 bytes)
├── price: f64        (8 bytes)
├── quantity: f64     (8 bytes)
└── side: Side        (1 byte + padding)

Trade (#[repr(C)], 40 bytes)
├── timestamp: i64    (8 bytes)
├── price: f64        (8 bytes)
├── quantity: f64     (8 bytes)
├── buyer_id: u64     (8 bytes)
└── seller_id: u64    (8 bytes)
```
