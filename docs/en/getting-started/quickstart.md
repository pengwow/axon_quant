# Quick Start

> Applicable version: AXON v0.2.0+
> Prerequisites: [Installation](installation.md)

This document takes you through running your first AXON backtest in 5 minutes.

## 1. Run Example

The repository includes multiple examples. Let's start with the most straightforward one:

```bash
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# Run random agent baseline (pure Python, no external dependencies)
PYTHONPATH=examples .venv/bin/python examples/02_rl_training/random_agent.py
```

Expected output:

```text
[random_agent] Running 5 random episodes, max 500 steps each
=== Random Strategy Baseline ===
  episodes        : 5
  mean_reward     : -0.1234
  mean_steps      : 500.0
  mean_final_value: 98765.43
  elapsed         : 0.15s
PASS: Random agent running normally
```

## 2. First Python Backtest (Optional)

```python
import axon_quant

# 1. Create synthetic market data
data = [
    {"timestamp": i, "open": 100.0, "high": 100.5, "low": 99.5,
     "close": 100.0, "volume": 1000.0}
    for i in range(500)
]

# 2. Create backtest engine
from axon_quant.backtest import L1MatchingEngine, limit_order

engine = L1MatchingEngine()

# 3. Submit orders
result = engine.submit(limit_order(1, "BTCUSDT", "Buy", 100.0, 1.0))
print(f"Order filled: {result['is_filled']}, Fills: {len(result['fills'])}")
```

## 3. RL Training Example

```bash
# Install RL dependencies
pip install axon_quant[rl]

# Run PPO training
PYTHONPATH=examples .venv/bin/python examples/02_rl_training/train_ppo.py \
    --timesteps 5000
```

## Next Steps

- [AI-Native Core Design](../user-guide/ai-native-design.md) — Understand the unified data pipeline
- [Architecture Overview](../user-guide/architecture.md) — System components and data flow
