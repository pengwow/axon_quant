# ADR 0002: Cargo Workspace Layout

## Status

Accepted

## Context

AXON consists of multiple crates with varying dependencies and feature requirements. Need a structure that:
- Allows independent compilation
- Supports feature flags for optional functionality
- Enables incremental builds
- Provides clear dependency hierarchy

## Decision

Use Cargo Workspace with 21 crates organized in 9 layers:

```
Layer 9: Application Entry (cli, python)
Layer 8: AI Agents (llm, explain)
Layer 7: Model Services (inference, ensemble)
Layer 6: Training Pipeline (rl, hpo, distributed, walk-forward)
Layer 5: Experiment Governance (tracker, registry)
Layer 4: Production Execution (exchange, risk, oms, monitor)
Layer 3: Backtesting Engine (backtest, compliance)
Layer 2: Data Services (data)
Layer 1: Core Types (core)
```

### Rationale

1. **Independent Compilation**: Each crate can be built independently
2. **Feature Flags**: Enable only required functionality
3. **Clear Dependencies**: Explicit dependency graph prevents circular dependencies
4. **Incremental Builds**: Only changed crates need recompilation

## Consequences

### Positive

- Fast incremental builds
- Clear module boundaries
- Easy to test individual crates
- Flexible deployment options

### Negative

- More complex configuration
- Need to manage feature flags carefully
- Cross-crate API changes require careful coordination

### Neutral

- All crates share the same Rust toolchain version
- Consistent coding style enforced across workspace
