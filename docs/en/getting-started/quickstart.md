# Quick Start

> Applicable version: AXON v0.3.0+
> Prerequisites: [Installation](installation.md)

This document takes you through running your first AXON backtest in 5 minutes.

## 1. Run Example

The repository includes 6 RL examples. Let's start with the most straightforward one:

```bash
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# Run L1 matching backtest example (pure Rust, no Python dependency)
cargo run -p axon-backtest --example simple_l1_backtest
```

Expected output:

```text
[INFO] axon-backtest started
[INFO] Loading market data: 1,000,000 ticks (50ms granularity)
[INFO] Matching engine: L1 (best price execution)
[INFO] Simulated orders: 100
[INFO] Average impact: 2.3 bps
[INFO] Total return: +12.4%
[INFO] Sharpe: 1.87
```

## 2. First Python Backtest (Optional)

```python
import axon_quant as aq
import numpy as np

# 1. Create synthetic market data
n_ticks = 100_000
prices = 100 + np.cumsum(np.random.randn(n_ticks) * 0.01)
volumes = np.random.uniform(100, 1000, n_ticks)

# 2. Create backtest environment
env = aq.make_env(
    market_data=aq.MarketData.from_arrays(prices, volumes),
    matching_engine="L1",
    impact_model="almgren_chriss",
    latency_model="fixed_1ms",
    fee_model="taker_5bps",
)

# 3. Run simple momentum strategy
position = 0
for tick in env:
    if tick.price > tick.sma(20):
        position = 1
    elif tick.price < tick.sma(20):
        position = -1
    env.submit_order(side="buy" if position > 0 else "sell", quantity=1)

# 4. Print results
result = env.run()
print(f"Total return: {result.total_return:.2%}")
print(f"Sharpe ratio: {result.sharpe_ratio:.2f}")
print(f"Max drawdown: {result.max_drawdown:.2%}")
```

## 3. LLM Trading Example

```python
import axon_quant as aq

# Create LLM trading agent
agent = aq.llm.ReActAgent(
    backend=aq.llm.OpenAICompatBackend(api_key="your-api-key"),
    tools=[
        aq.llm.PlaceOrderTool(),
        aq.llm.QueryPortfolioTool(),
    ],
    safety_mode="two_phase",
)

# Run trading loop
for market_state in env:
    decision = agent.decide(market_state)
    if decision.confidence > 0.7:
        env.execute(decision.action)
```

## Next Steps

- [AI-Native Core Design](../user-guide/ai-native-design.md) — Understand the unified data pipeline
- [Architecture Overview](../user-guide/architecture.md) — System components and data flow
