# RL Training Guide

This guide explains how to use axon_quant's reinforcement learning (RL) functionality to train trading strategies.

## Quick Start

### 1. Install Dependencies

```bash
# Basic install (runtime only, no training)
pip install axon_quant

# With RL training dependencies (gymnasium, stable-baselines3, torch)
pip install axon_quant[rl]
```

### 2. Run Random Baseline (No sb3 Required)

```bash
cd axon
PYTHONPATH=examples .venv/bin/python examples/02_rl_training/random_agent.py
```

### 3. Run PPO Training (Requires sb3)

```bash
PYTHONPATH=examples .venv/bin/python examples/02_rl_training/train_ppo.py \
    --timesteps 5000 --n-envs 1
```

---

## Environment Configuration

`TradingEnv` is configured via a dictionary:

```python
import axon_quant

config = {
    "initial_capital": 100_000.0,   # Initial capital
    "transaction_cost": 0.001,      # Transaction fee rate
    "slippage": 0.0001,             # Slippage
    "max_steps": 500,               # Max steps per episode
    "seed": 42,                     # Random seed
    "symbol": "BTCUSDT",           # Trading pair
    "return_window": 50,            # Return window (for Sharpe/Sortino)
}

env = axon_quant.rl.TradingEnv(
    config=config,
    action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    market_data=market_data,
    reward="pnl",
)
```

### Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `initial_capital` | float | 100000 | Initial capital |
| `transaction_cost` | float | 0.001 | Transaction fee rate (0.1%) |
| `slippage` | float | 0.0001 | Slippage (0.01%) |
| `max_steps` | int | 500 | Max steps per episode |
| `seed` | int | 42 | Random seed (reproducible) |
| `symbol` | str | "BTCUSDT" | Trading pair name |
| `return_window` | int | 50 | Sharpe/Sortino calculation window |

---

## Reward Functions

axon_quant includes three built-in reward functions:

### pnl — Absolute PnL

```python
env = axon_quant.rl.TradingEnv(config=config, reward="pnl", ...)
```

- Calculation: Net asset value change per step
- Use case: Simple and intuitive, good for beginners
- Note: Does not consider risk, may produce high-volatility strategies

### sharpe — Rolling Sharpe Ratio

```python
env = axon_quant.rl.TradingEnv(config=config, reward="sharpe", ...)
```

- Calculation: Sharpe ratio within rolling window
- Use case: Risk-adjusted return optimization
- Note: Default `clip=10.0` prevents extreme values from causing gradient explosion

### sortino — Rolling Sortino Ratio

```python
env = axon_quant.rl.TradingEnv(config=config, reward="sortino", ...)
```

- Calculation: Return ratio considering only downside risk
- Use case: Scenarios focused on loss risk
- Note: Does not penalize upside volatility

### Selection Guide

| Scenario | Recommended Reward |
|----------|-------------------|
| Quick validation | `pnl` |
| Robust strategy | `sharpe` |
| Risk-averse | `sortino` |

---

## Integration with stable-baselines3

### PPO Training Example

```python
from stable_baselines3 import PPO
from axon_examples.vec_env import AxonTradingEnv, make_vec_env
from axon_examples.common import make_env, make_env_config, make_synthetic_market_data

# 1. Prepare data
market_data = make_synthetic_market_data(n=500, seed=42)
config = make_env_config(max_steps=500, seed=42)

# 2. Create environment factory
def env_fn():
    return AxonTradingEnv(make_env(config=config, market_data=market_data, reward="sharpe"))

# 3. Create vectorized environment
venv = make_vec_env(env_fn, n_envs=4, use_stable_baselines3=True)

# 4. Create model
model = PPO("MlpPolicy", venv, verbose=1, learning_rate=3e-4)

# 5. Train
model.learn(total_timesteps=50_000)

# 6. Save model
model.save("ppo_trading")
```

### SAC Training Example

```python
from stable_baselines3 import SAC

model = SAC(
    "MlpPolicy",
    venv,
    verbose=1,
    learning_rate=3e-4,
    buffer_size=10_000,
    batch_size=256,
)
model.learn(total_timesteps=50_000)
```

---

## Multi-Environment Parallel Training

Use `make_vec_env` to create multiple parallel environments:

```python
from axon_examples.vec_env import make_vec_env

# Create 4 parallel environments
venv = make_vec_env(env_fn, n_envs=4, use_stable_baselines3=True)

# Or use async environment (multi-process)
venv = make_vec_env(env_fn, n_envs=4, use_async=True)
```

### Performance Comparison

```bash
# Run comparison experiment
PYTHONPATH=examples .venv/bin/python examples/02_rl_training/vec_env_train.py \
    --n-envs 4 --timesteps 5000 --compare-with-serial
```

---

## Custom Reward Functions

Reward functions are currently implemented in Rust. To customize:

1. **Modify Rust code**: Add new implementation in `crates/axon-rl/src/reward/`
2. **Use Python wrapper**: Post-process reward in Python

```python
class CustomRewardWrapper:
    """Post-process raw reward. """
    def __init__(self, env, alpha=0.5):
        self._env = env
        self._alpha = alpha
        self._prev_value = None

    def step(self, action):
        result = self._env.step(action)
        obs, reward, terminated, truncated, info = result
        # Custom logic: combine PnL and position change
        custom_reward = self._alpha * reward + (1 - self._alpha) * info.get("position_change", 0)
        return obs, custom_reward, terminated, truncated, info
```

---

## Complete Training Pipeline

```python
"""Complete PPO training + evaluation pipeline. """
import time
from stable_baselines3 import PPO
from axon_examples.vec_env import AxonTradingEnv, make_vec_env
from axon_examples.common import (
    make_env, make_env_config, make_synthetic_market_data,
    run_random_episode, set_seed,
)

set_seed(42)

# Data preparation
market_data = make_synthetic_market_data(n=500, seed=42)
config = make_env_config(max_steps=500, seed=42)

def env_fn():
    return AxonTradingEnv(make_env(config=config, market_data=market_data, reward="pnl"))

# Training
venv = make_vec_env(env_fn, n_envs=1)
model = PPO("MlpPolicy", venv, verbose=0, learning_rate=3e-4, n_steps=512)

t0 = time.perf_counter()
model.learn(total_timesteps=10_000)
print(f"Training time: {time.perf_counter() - t0:.1f}s")

# Evaluation
obs = venv.reset()
total_reward = 0
for _ in range(500):
    action, _ = model.predict(obs, deterministic=True)
    obs, reward, done, info = venv.step(action)
    total_reward += reward
    if done:
        break

print(f"Strategy reward: {total_reward:.2f}")

# Compare with random
env = env_fn()
random_result = run_random_episode(env, max_steps=500, seed=42)
print(f"Random reward: {random_result['total_reward']:.2f}")
```

---

## FAQ

### Q: "Missing RL training dependencies" message appears

```bash
pip install gymnasium stable-baselines3 torch
```

Or use optional dependencies:

```bash
pip install axon_quant[rl]
```

### Q: Training is slow

1. Increase parallel environments: `n_envs=4` or more
2. Use GPU: `pip install torch --index-url https://download.pytorch.org/whl/cu121`
3. Reduce `max_steps`: Quick iteration validation

### Q: How to use real data

```python
import pandas as pd

# Read from CSV
df = pd.read_csv("btc_1h.csv")
market_data = df[["timestamp", "open", "high", "low", "close", "volume"]].to_dict("records")

env = axon_quant.rl.TradingEnv(config=config, market_data=market_data, reward="sharpe")
```

### Q: How to deploy the model

```python
# Load trained model
model = PPO.load("ppo_trading")

# Real-time prediction
obs = env.reset()
action, _ = model.predict(obs, deterministic=True)
```

---

## Related Documentation

- [PPO Training Script](../../../examples/02_rl_training/train_ppo.py)
- [SAC Training Script](../../../examples/02_rl_training/train_sac.py)
- [Vectorized Training Example](../../../examples/02_rl_training/vec_env_train.py)
- [Reward Function Comparison](../../../examples/02_rl_training/custom_reward.py)
