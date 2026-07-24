# RL Training User Guide (0.9.0)

This guide covers RL environment wrappers, strategy abstractions, ONNX deployment, and HPO training shipped in AXON 0.9.0 (`BacktestEnv` / `MultiLegBacktestEnv` / `L3BookDiff` streaming / `OnnxPolicyStrategy` / `RLHPOSweeper`).

---

## Quick Start

```bash
# 1. Install RL dependencies
uv pip install "axon-quant[rl,onnx]"

# 2. Train spot single-leg PPO 50K (SB3 path)
uv run python examples/rl/train_spot_single_leg.py

# 3. Train spot+perp arbitrage 100K (main acceptance demo)
uv run python examples/rl/spot_perp_arb_demo.py

# 4. 8-CPU parallel HPO sweep (100 trials)
uv run python examples/rl/hpo_spot_perp_demo.py --n-trials 100 --n-jobs 8
```

---

## Core Concepts

AXON's RL training stitches the **backtest engine**, **L3 order book streaming**, **PyO3 bindings**, **SB3/RLLib training**, **ONNX deployment**, and **Optuna HPO** into a single end-to-end pipeline.

```
   ┌─────────────────┐  env.step   ┌──────────────┐  export  ┌─────────┐
   │  BacktestEngine │ ────────────│  SB3/RLLib   │ ────────│  ONNX   │
   │  (Rust core)     │             │  (train loop) │         │ policy  │
   └─────────────────┘             └──────────────┘         └─────────┘
          │                              │                      │
          │  L3BookDiff (per_bar)        │                      │
          │  ──────────────────►         │                      ▼
          │                              │            ┌───────────────────┐
          │                              │            │ OnnxPolicyStrategy│
          │                              │            │ (Python deploy)   │
          │                              │            └───────────────────┘
          ▼                              ▼                      │
   ┌─────────────────┐  best_params  ┌──────────────┐           │
   │  OptunaHPO      │ ─────────────│  RLHPOSweeper│           │
   │  (8-CPU parallel)│             │  (Python glue)│           │
   └─────────────────┘             └──────────────┘           │
                                                            ▼
                                                ┌───────────────────┐
                                                │  BacktestEngine   │
                                                │  (production sim) │
                                                └───────────────────┘
```

---

## API Overview

### `BacktestEnv` (D1.1)

Wraps `BacktestEngine` as a `gym.Env` protocol for single-leg training.

```python
from axon_quant.backtest import spot_instrument
from axon_quant.env import BacktestEnv

spot = spot_instrument("BTC", "USDT")
env = BacktestEnv(spot, seed=42)
obs, info = env.reset(seed=42)
obs, reward, term, trunc, info = env.step(env.action_space.sample())
```

**Field notes**:
- `observation_space`: `Box(shape=(OBS_DIM_SINGLE_LEG,))` — contains last mid price, volume, cash, position
- `action_space`: `Box(low=-1.0, high=1.0, shape=(1,))` — normalized target position
- `reset()`: reset `BacktestEngine` + run first bar to construct obs
- `step(action)`: translate action to order → engine.run() → next bar obs + PnL reward

### `MultiLegBacktestEnv` (D1.2)

Multi-leg synchronous observation (2-3 legs; main demo uses 2 legs: spot + perp arbitrage).

```python
from axon_quant.backtest import spot_instrument, swap_instrument
from axon_quant.env import MultiLegBacktestEnv

spot = spot_instrument("BTC", "USDT")
perp = swap_instrument("BTC", "USDT")
env = MultiLegBacktestEnv(
    [(spot, 1.0), (perp, 1.0)],
    seed=42,
)
```

Per-leg observations are concatenated into `(OBS_DIM_SINGLE_LEG * n_legs,)` `Box`; same for actions.

### `L3BookDiff` Streaming Subscription (C2.1, new in 0.9.0)

Subscribe to L3 order book deltas for training visualization, CB monitoring, and shadow strategy validation.

```python
from axon_quant.backtest import BacktestEngine

engine = BacktestEngine(initial_cash=100_000.0)

def my_callback(diff):
    print(f"L3 diff @ {diff['timestamp_ns']}: +{len(diff['added'])} -{len(diff['removed'])}")

sub_id = engine.subscribe(callback=my_callback, kind="per_bar")
# ... run sim ...
engine.unsubscribe(sub_id)
```

**`kind` options**:
- `"per_bar"`: push diff at end of each bar (training visualization)
- `"per_fill"`: push diff on each fill (high-freq replay / microstructure analysis)
- `"both"`: push at both timings (use with caution; may double-count)

### `BaseStrategy` ABC (C3.1)

Python-side strategy abstraction mirroring Rust `StreamingStrategy` trait.

```python
from axon_quant.strategy import BaseStrategy

class MyStrategy(BaseStrategy):
    def on_bar(self, bar, ctx):
        # Required: receive bar + ctx (BarContext), return order list
        return []

    def on_fill(self, fill, ctx):
        # Optional: fill-triggered, default empty
        return []
```

### `OnnxPolicyStrategy` (D1.4c)

Deployment: load ONNX policy → BacktestEngine decision.

```python
from pathlib import Path
from axon_quant.strategy import OnnxPolicyStrategy

strategy = OnnxPolicyStrategy(
    onnx_path=Path("artifacts/spot_perp_arb.onnx"),
    leg_specs=[(spot, 1.0), (perp, 1.0)],
    providers=["CPUExecutionProvider"],  # or ["CUDAExecutionProvider"]
)
action = strategy.predict(obs_sample)  # shape = (n_legs,)
```

### `RLHPOSweeper` (D1.5a)

Optuna HPO glue with 8-CPU parallel support for 100 trials.

```python
from axon_quant.training import RLHPOSweeper

sweeper = RLHPOSweeper(
    study_name="my_hpo",
    n_trials=100,
    n_jobs=8,                              # 8-CPU parallel
    storage="sqlite:///optuna.db",         # cross-process sync
)
best = sweeper.sweep(objective_fn=my_objective)
print(f"best params: {best}")
```

---

## Custom HPO Search Space

Default search space is PPO 4-dim (lr / gamma / clip_param / entropy_coeff). Customize:

```python
from axon_hpo.search_space import SearchSpaceDef
from axon_quant.training import RLHPOSweeper

custom_space = {
    "lr": SearchSpaceDef(param_type="log_uniform", low=1e-5, high=1e-3),
    "n_steps": SearchSpaceDef(param_type="categorical", choices=[512, 1024, 2048, 4096]),
    "batch_size": SearchSpaceDef(param_type="categorical", choices=[32, 64, 128, 256]),
    "gae_lambda": SearchSpaceDef(param_type="uniform", low=0.9, high=0.99),
}

sweeper = RLHPOSweeper(
    study_name="custom_hpo",
    n_trials=50,
    search_space=custom_space,
    n_jobs=4,
)
```

`SearchSpaceDef` supported `param_type`:
- `log_uniform`: log-uniform (good for lr, entropy)
- `uniform`: linear uniform
- `categorical`: discrete choices
- `int`: integer

---

## TensorBoard Integration

Each trial writes to its own directory for multi-trial comparison:

```python
from axon_quant.training.hpo_sweeper import make_tb_log_dir

def objective(params):
    tb_dir = make_tb_log_dir(trial_id=current_trial_id, base="./tb_logs")
    model = PPO("MlpPolicy", env, verbose=0, tensorboard_log=tb_dir, **params)
    model.learn(total_timesteps=50_000)
    return [sharpe_ratio]
```

Launch TensorBoard:

```bash
tensorboard --logdir ./tb_logs/
# Visit http://localhost:6006
```

---

## 0.8.0 → 0.9.0 API Changes

| 0.8.0 | 0.9.0 (branch) | Change |
|-------|----------------|--------|
| No `BacktestEnv` | `python/axon_quant/env.py` | Added `gym.Env` wrapper |
| No `L3BookDiff` | `BacktestEngine::subscribe()` | Added streaming subscription |
| No `BaseStrategy` ABC | `python/axon_quant/strategy/base.py` | Added Python strategy abstraction |
| No `OnnxPolicyStrategy` | `python/axon_quant/strategy/onnx_policy.py` | Added ONNX deployment adapter |
| No `RLHPOSweeper` | `python/axon_quant/training/hpo_sweeper.py` | Added Optuna glue |
| `Action` (5-class discrete) | `MultiLegAction` (`axon-inference::types`) | Added multi-leg continuous action |

0.9.0 covers all 19 plan tasks (see `docs/superpowers/plans/2026-07-22-axon-quant-0.9.0-rl-training.md`).

---

## Main Acceptance Metrics

| Metric | Target | Failure Standard |
|--------|--------|------------------|
| Training convergence | Sharpe > 1.0 (100K timesteps) | 100K not converge -> tune reward / obs |
| HPO gain | best vs baseline Sharpe +20% | < 10% -> expand search space |
| ONNX e2e | sim PnL error < 5% | > 10% -> float / schema drift |
| HPO performance | 100 trial 8-CPU <= 3h | > 3h -> shrink search space |

Actual run results pending (`docs/superpowers/notes/2026-07-22-rl-main-acceptance.md`).

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `ImportError: stable_baselines3` | RL extra not installed | `uv pip install axon-quant[rl]` |
| `ONNX export shape mismatch` | obs/action dim mismatch | Check `observation_space.shape == model.policy.obs_dim` |
| HPO trial slow | objective instantiates env too many times | Move env to module-level, only reset in `objective` |
| L3BookDiff not triggered | subscribe called after `engine.run()` | subscribe before run |
| `n_jobs > 1` pickle error | objective_fn closure holds unpicklable | Move state to module-level |
