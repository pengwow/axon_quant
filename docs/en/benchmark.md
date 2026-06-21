# Benchmark Report

This document presents the performance benchmark results for the AXON quantitative trading framework.

## Test Environment

- **Operating System**: macOS (Apple Silicon)
- **Rust Version**: 1.96.0+
- **Build Mode**: Release (optimized)

## Core Performance Metrics

| Metric | Result | Description |
|--------|--------|-------------|
| Event Builder Tick | ~2.4 ns | Single tick event construction latency |
| Event Builder Bar | ~2.2 ns | Single bar event construction latency |
| Impact Linear | ~3.2 ns | Linear impact model calculation |
| Reward PnL | ~1.2 ns | PnL reward calculation |
| Reward Sharpe | ~111 ns | Sharpe ratio calculation |

## Detailed Report

Run the following command to generate a full HTML benchmark report locally:

```bash
make bench-report
```

The report will be generated to the `target/criterion/` directory.

## Running Benchmarks

```bash
# Run all benchmarks
make bench

# Run benchmarks for a specific crate
cargo bench -p axon-core

# Run specific benchmark
cargo bench -p axon-core -- event_builder_tick

# Generate reports to docs directory
make bench-report
```

## Benchmark Descriptions

### axon-core

- **event_builder_tick**: Tests single tick event construction performance
- **event_builder_bar**: Tests single bar event construction performance
- **impact_linear**: Tests linear impact model calculation performance
- **reward_pnl**: Tests PnL reward function calculation performance
- **reward_sharpe**: Tests Sharpe ratio reward function calculation performance

### axon-backtest

- **matching_latency**: Tests matching engine latency performance
- **order_book**: Tests order book operation performance

### axon-rl

- **observation**: Tests observation space construction performance
- **trading_env**: Tests trading environment step/reset performance
