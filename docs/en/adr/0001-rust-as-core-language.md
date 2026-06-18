# ADR 0001: Rust as Core Language

## Status

Accepted

## Context

AXON is a high-performance quantitative trading framework that requires:
- Nanosecond-level timestamp precision
- Deterministic order matching
- Zero-cost abstractions for trading logic
- Memory safety without garbage collection
- High throughput (> 1M events/sec)

## Decision

Use Rust as the core implementation language for all performance-critical components.

### Rationale

1. **Performance**: Rust provides C/C++ level performance with zero-cost abstractions
2. **Memory Safety**: Ownership system prevents data races and memory leaks
3. **Concurrency**: Built-in async/await and Send/Sync traits for safe concurrent code
4. **Ecosystem**: Rich crate ecosystem for networking, serialization, and numerical computing
5. **Python Integration**: PyO3 provides seamless Python bindings

## Consequences

### Positive

- Achieved > 1M events/sec backtesting throughput
- P99 matching latency < 1μs
- Zero memory safety issues in production
- Single codebase for backtesting and live trading

### Negative

- Steeper learning curve for Python developers
- Longer compilation times compared to Python
- Smaller developer pool compared to Python/Java

### Neutral

- Python bindings via PyO3 for ease of use
- Documentation and examples in both Rust and Python
