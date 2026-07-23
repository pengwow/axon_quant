# RL Training User Guide (0.9.0)

## Quick Start

```bash
# 1. Install RL dependencies
uv pip install "axon-quant[rl,onnx]"

# 2. Train spot single-leg PPO 50K
uv run python examples/rl/train_spot_single_leg.py

# 3. Export ONNX + deploy to BacktestEngine
uv run python examples/rl/spot_perp_arb_demo.py
```

## API Overview

### `BacktestEnv`

Wraps `BacktestEngine` as a `gym.Env` protocol.

```python
from axon_quant.backtest import spot_instrument
from axon_quant.env import BacktestEnv

spot = spot_instrument("BTC", "USDT")
env = BacktestEnv(spot, seed=42)
obs, info = env.reset(seed=42)
obs, reward, term, trunc, info = env.step(env.action_space.sample())
```

### `MultiLegBacktestEnv`

Multi-leg synchronous observation/action (2-3 legs; main demo uses 2 legs: spot + perp arbitrage).

```python
from axon_quant.env import MultiLegBacktestEnv

env = MultiLegBacktestEnv([
    (spot, 1.0),
    (perp, 1.0),
], seed=42)
```

### `OnnxPolicyStrategy`

Deployment: load ONNX policy → BacktestEngine decision.

```python
from axon_quant.strategy import OnnxPolicyStrategy
strategy = OnnxPolicyStrategy(
    onnx_path=Path("artifacts/spot_perp_arb.onnx"),
    leg_specs=[(spot, 1.0), (perp, 1.0)],
)
```

### `RLHPOSweeper`

Optuna HPO glue with 8-CPU parallel support for 100 trials.

```python
from axon_quant.training import RLHPOSweeper

sweeper = RLHPOSweeper(
    study_name="my_hpo",
    n_trials=100,
    n_jobs=8,
    storage="sqlite:///optuna.db",
)
best = sweeper.sweep(objective_fn=my_objective)
```

## Main Acceptance Metrics

| Metric | Target | Failure Standard |
|--------|--------|------------------|
| Training convergence | Sharpe > 1.0 (100K timesteps) | 100K not converge -> tune reward / obs |
| HPO gain | best vs baseline Sharpe +20% | < 10% -> expand search space |
| ONNX e2e | sim PnL error < 5% | > 10% -> float / schema drift |
| HPO performance | 100 trial 8-CPU <= 3h | > 3h -> shrink search space |
