# Architecture Overview

> Applicable version: AXON v0.1.0+

This document describes AXON's system architecture and data flow.

## System Overview

```text
┌─────────────────────────────────────────────────────────────────────┐
│                          AXON System Architecture                    │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐             │
│  │   Phase 1    │    │   Phase 2    │    │   Phase 3    │             │
│  │  Core Engine │    │  Training    │    │  AI Enhanced │             │
│  └──────┬──────┘    └──────┬──────┘    └──────┬──────┘             │
│         │                  │                  │                     │
│  ┌──────▼──────┐    ┌──────▼──────┐    ┌──────▼──────┐             │
│  │ axon-core   │    │ axon-hpo    │    │ axon-llm    │             │
│  │ axon-backtest│    │ axon-walk-  │    │ axon-explain│             │
│  │ axon-rl     │    │   forward   │    │ axon-       │             │
│  │             │    │ axon-tracker│    │   ensemble  │             │
│  │             │    │ axon-registry│   │ axon-data   │             │
│  │             │    │ axon-       │    │ axon-       │             │
│  │             │    │   distributed│   │   compliance│             │
│  └─────────────┘    └─────────────┘    └─────────────┘             │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │                      Phase 4: Production                     │   │
│  │  ┌───────────┐ ┌───────────┐ ┌───────────┐ ┌───────────┐   │   │
│  │  │axon-risk  │ │axon-      │ │axon-      │ │axon-oms   │   │   │
│  │  │Risk Engine│ │inference  │ │exchange   │ │Order Mgmt │   │   │
│  │  │           │ │Inference  │ │Exchange   │ │           │   │   │
│  │  └─────┬─────┘ └─────┬─────┘ └─────┬─────┘ └─────┬─────┘   │   │
│  │        │             │             │             │           │   │
│  │        └─────────────┼─────────────┼─────────────┘           │   │
│  │                      │             │                         │   │
│  │                ┌─────▼─────────────▼─────┐                   │   │
│  │                │     axon-monitor        │                   │   │
│  │                │     Monitoring          │                   │   │
│  │                └─────────────────────────┘                   │   │
│  └─────────────────────────────────────────────────────────────┘   │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

## Data Flow

### Backtesting Data Flow

```text
Market Data File
    │
    ▼
┌─────────────┐
│  Data Load   │  axon-data
│  (Arrow/     │
│   Parquet)   │
└──────┬──────┘
       │
       ▼
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  Event Queue │────▶│  Strategy   │────▶│  Order Event│
│  (BinaryHeap)│     │  (RL/Rule)  │     │             │
└─────────────┘     └─────────────┘     └──────┬──────┘
                                               │
                                               ▼
                                        ┌─────────────┐
                                        │  Matching    │  axon-backtest
                                        │  (L1/L2/L3) │
                                        └──────┬──────┘
                                               │
                                               ▼
                                        ┌─────────────┐
                                        │  Fill Event  │
                                        └──────┬──────┘
                                               │
                                               ▼
                                        ┌─────────────┐
                                        │  Portfolio   │  axon-core
                                        │  (Position)  │
                                        └─────────────┘
```

### Live Trading Data Flow

```text
Exchange API
    │
    ▼
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  axon-      │────▶│  axon-risk  │────▶│  axon-oms   │
│  exchange   │     │  Risk Check │     │  Order Mgmt │
│  (WebSocket │     │  (12ns)     │     │  (1.2µs)    │
│   + REST)   │     └─────────────┘     └──────┬──────┘
└─────────────┘                                │
                                               ▼
                                        ┌─────────────┐
                                        │  axon-      │
                                        │  exchange   │
                                        │  (Submit)   │
                                        └──────┬──────┘
                                               │
                                               ▼
                                        ┌─────────────┐
                                        │  axon-      │
                                        │  monitor    │
                                        │  (Metrics)  │
                                        └─────────────┘
```

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

## Concurrency Model

```text
┌─────────────────────────────────────────────────────────┐
│                    Main Thread (Event Loop)               │
│                                                         │
│  ┌─────────────┐     ┌─────────────┐     ┌───────────┐ │
│  │ Event Queue │────▶│ Event Router│────▶│ Handlers  │ │
│  │ (crossbeam) │     │             │     │ (sync)    │ │
│  └─────────────┘     └─────────────┘     └───────────┘ │
└─────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────┐
│                    I/O Thread Pool (tokio)                │
│                                                         │
│  ┌─────────────┐     ┌─────────────┐     ┌───────────┐ │
│  │ WebSocket   │     │ REST API    │     │ File Watch│ │
│  │ Connection  │     │ Request     │     │ (notify)  │ │
│  └─────────────┘     └─────────────┘     └───────────┘ │
└─────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────┐
│                    Compute Thread Pool (rayon)            │
│                                                         │
│  ┌─────────────┐     ┌─────────────┐     ┌───────────┐ │
│  │ Batch       │     │ VaR         │     │ Data      │ │
│  │ Inference   │     │ Computation │     │ Processing│ │
│  └─────────────┘     └─────────────┘     └───────────┘ │
└─────────────────────────────────────────────────────────┘
```

## Next Steps

- [AI-Native Core Design](ai-native-design.md) — Deep dive into the unified data pipeline
- [Strategy Development](strategy-development.md) — Complete strategy development workflow
- [LLM Trading Architecture](llm-trading/overview.md) — LLM trading system architecture
