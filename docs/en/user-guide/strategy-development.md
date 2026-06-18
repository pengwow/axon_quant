# Scenario 1 — Strategy Development Pipeline

This document demonstrates how to complete a full strategy development pipeline on the AXON quantitative platform, covering 7 key steps from data preparation to model deployment. Each step is based on AXON `0.1.0` real source code and shows "how the output of one step becomes the input of the next".

---

## Pipeline Overview

```text
+-----------+     +-----------+     +-----------+     +-----------+
|  1. Data  | --> | 2. RL     | --> | 3. HPO    | --> | 4. Walk-  |
| Preparation|    | Training  |     | Search    |     | Forward   |
+-----------+     +-----------+     +-----------+     +-----------+
                                              |
                                              v
+-----------+     +-----------+     +-----------+
| 7. Model  | <-- | 6. Model  | <-- | 5. Backtest|
| Registry  |     | Export    |     | Validation |
+-----------+     +-----------+     +-----------+
```

---

## Step 1: Data Preparation (Feature Engineering)

**Input**: Raw market data (CSV / Parquet / live stream)
**Output**: Standardized `MarketBar` sequence + `FeatureConfig` feature configuration

AXON's data layer (`axon-data`) supports CSV, Parquet, and memory-mapped sources. The following example demonstrates how to construct synthetic data and configure observation features for subsequent RL environments.

```python
"""
Step 1: Data Preparation and Feature Engineering
- Generate/load K-line data
- Define observation space feature configuration (close, volume, RSI, etc.)
- Output: market_data + features, as input to TradingEnv
"""

from __future__ import annotations

import numpy as np

# -------------------------------------------------
# 1.1 Synthetic Data Generation (zero external dependencies, CI-friendly)
# -------------------------------------------------
def make_synthetic_market_data(n: int = 500, seed: int = 42) -> list[dict]:
    """Generate random walk synthetic K-lines for rapid prototyping."""
    rng = np.random.default_rng(seed)
    price = 100.0
    bars = []
    for i in range(n):
        ret = rng.normal(0.0, 0.02)
        open_p = price * (1 + ret)
        high_p = open_p * (1 + abs(rng.normal(0, 0.01)))
        low_p = open_p * (1 - abs(rng.normal(0, 0.01)))
        close_p = (high_p + low_p) / 2 + rng.normal(0, 0.005)
        vol = rng.uniform(1000.0, 5000.0)
        bars.append({
            "timestamp": i,
            "open": open_p,
            "high": high_p,
            "low": low_p,
            "close": close_p,
            "volume": vol,
        })
        price = close_p
    return bars


# -------------------------------------------------
# 1.2 Feature Configuration: Define which fields the ObservationSpace extracts
# -------------------------------------------------
def make_feature_config():
    """Construct feature configuration list, determining what the RL agent sees."""
    return [
        {
            "name": "close",
            "source": {"PriceField": "close"},   # Extract from bar.close
            "normalizer": "ZScore",               # Z-Score normalization
            "clip_range": None,
        },
        {
            "name": "volume",
            "source": {"VolumeField": "volume"},
            "normalizer": "None",                 # No normalization
            "clip_range": None,
        },
        {
            "name": "returns",
            "source": {"Derived": "returns"},    # Derived feature: returns
            "normalizer": "ZScore",
            "clip_range": (-3.0, 3.0),            # Clip extreme values
        },
    ]


# -------------------------------------------------
# 1.3 Environment Configuration: Bind data with features
# -------------------------------------------------
def make_env_config(
    initial_capital: float = 100_000.0,
    max_steps: int = 500,
    seed: int = 42,
) -> dict:
    """Construct TradingEnv configuration dictionary."""
    return {
        "symbol": "BTCUSDT",
        "initial_capital": initial_capital,
        "max_steps": max_steps,
        "seed": seed,
        "return_window": 20,          # Return history window (for Sharpe/Sortino reward)
    }


# Main entry point: output market_data + feature_config + env_config
if __name__ == "__main__":
    market_data = make_synthetic_market_data(n=500, seed=42)
    features = make_feature_config()
    cfg = make_env_config()
    print(f"[Step 1] Generated {len(market_data)} K-lines, {len(features)} features")
    # Next step: Pass these data to TradingEnv and VecEnv for RL training
```

**Previous step output → Next step input**: `market_data` + `features` + `cfg` as parameters to `TradingEnv::new()`.

---

## Step 2: RL Training (PPO + TradingEnv + VecEnv)

**Input**: `market_data`, `features`, `cfg`
**Output**: Trained PPO model (`model.zip`) + training logs

AXON's RL layer (`axon-rl`) provides `TradingEnv` (Gymnasium compatible) and `SyncVecEnv` / `AsyncVecEnv` (vectorized parallel). The following example uses `stable-baselines3`'s PPO algorithm for training.

```python
"""
Step 2: RL Training (PPO + TradingEnv + VecEnv)
- Use previous step's market_data / cfg to construct TradingEnv
- Parallelize training via VecEnv
- Output: Trained model file path
"""

from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

# Assume stable-baselines3 is installed
try:
    from stable_baselines3 import PPO
    from stable_baselines3.common.callbacks import BaseCallback
except ImportError:
    print("ERROR: requires `stable-baselines3`. Run: pip install stable-baselines3 gymnasium torch")
    sys.exit(2)

import _vec_env
import _common


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="PPO training example")
    p.add_argument("--timesteps", type=int, default=5000, help="Total training timesteps")
    p.add_argument("--n-envs", type=int, default=4, help="Number of parallel environments")
    p.add_argument("--n-bars", type=int, default=500, help="Number of synthetic K-lines")
    p.add_argument("--learning-rate", type=float, default=3e-4)
    p.add_argument("--batch-size", type=int, default=64)
    p.add_argument("--n-steps", type=int, default=512, help="PPO sampling steps per update")
    p.add_argument("--gamma", type=float, default=0.99)
    p.add_argument("--seed", type=int, default=42)
    p.add_argument("--save-path", type=Path, default=Path("models/ppo_momentum.zip"))
    p.add_argument("--reward", choices=("pnl", "sharpe", "sortino"), default="pnl")
    return p.parse_args()


class ProgressCallback:
    """Minimal callback: print training status every log_every steps."""

    def __init__(self, log_every: int = 500) -> None:
        self.log_every = log_every
        self.episode_rewards: list[float] = []

    def __call__(self, locals_dict, globals_dict) -> bool:
        infos = locals_dict.get("infos", [])
        for info in infos:
            r = info.get("episode")
            if r is not None:
                self.episode_rewards.append(r["r"])
        n_calls = locals_dict.get("self").num_timesteps
        if n_calls % self.log_every == 0 and self.episode_rewards:
            mean_r = sum(self.episode_rewards[-20:]) / min(20, len(self.episode_rewards))
            print(f"  step={n_calls:>6}  ep_rew_mean(20)={mean_r:>10.2f}")
        return True


def _build_factory(n_bars: int, base_seed: int, env_id: int, reward: str):
    """Factory function: each environment uses independent seed offset to avoid identical data."""
    market_data = _common.make_synthetic_market_data(n=n_bars, seed=base_seed + env_id)
    cfg = _common.make_env_config(
        max_steps=n_bars, seed=base_seed + env_id, symbol=f"BTCUSDT_{env_id}"
    )

    def _factory():
        return _vec_env.AxonTradingEnv(
            _common.make_env(config=cfg, market_data=market_data, reward=reward)
        )

    return _factory


def main() -> int:
    args = parse_args()
    _common.set_seed(args.seed)

    print(f"[Step 2] Preparing {args.n_bars} synthetic K-lines, {args.n_envs} parallel environments, reward={args.reward}")

    # -------------------------------------------------
    # 2.1 Construct vectorized environment (SyncVecEnv / DummyVecEnv)
    # -------------------------------------------------
    factories = [_build_factory(args.n_bars, args.seed, i, args.reward) for i in range(args.n_envs)]
    venv = _vec_env.make_vec_env(lambda: factories[0](), n_envs=args.n_envs, use_stable_baselines3=True)
    print(f"  vec env: {type(venv).__name__}, num_envs={venv.num_envs}")

    # -------------------------------------------------
    # 2.2 Construct PPO model
    # -------------------------------------------------
    model = PPO(
        "MlpPolicy",
        venv,
        verbose=0,
        learning_rate=args.learning_rate,
        n_steps=args.n_steps,
        batch_size=args.batch_size,
        gamma=args.gamma,
        seed=args.seed,
    )

    # -------------------------------------------------
    # 2.3 Training
    # -------------------------------------------------
    print(f"[Step 2] Starting training for {args.timesteps} steps ...")
    t0 = time.perf_counter()
    cb = ProgressCallback(log_every=500)
    try:
        model.learn(total_timesteps=args.timesteps, callback=cb, progress_bar=False)
    except TypeError:
        model.learn(total_timesteps=args.timesteps, callback=cb)
    elapsed = time.perf_counter() - t0
    print(f"[Step 2] Training complete, elapsed {elapsed:.1f}s")

    # -------------------------------------------------
    # 2.4 Save model (as input for next step HPO / backtest)
    # -------------------------------------------------
    args.save_path.parent.mkdir(parents=True, exist_ok=True)
    model.save(str(args.save_path))
    print(f"[Step 2] Model saved to {args.save_path}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

**Previous step output → Next step input**: `args.save_path` (model file path) passed to HPO's objective function for evaluating model performance under different hyperparameters.

---

## Step 3: HPO Hyperparameter Search (OptunaStudy + Multi-Objective Optimization)

**Input**: Model training script path + search space definition
**Output**: Pareto frontier (multi-objective) or best trial (single-objective) + optimal hyperparameter dictionary

AXON's HPO layer (`axon-hpo`) wraps Optuna, supporting single and multi-objective optimization with built-in Pareto frontier and hypervolume calculation.

```python
"""
Step 3: HPO Hyperparameter Search (OptunaStudy + Multi-Objective Optimization)
- Define search space (learning rate, gamma, batch_size, etc.)
- Objective function: Load step 2's model framework, train with different hyperparameters
  and evaluate Sharpe ratio + max drawdown
- Output: Optimal hyperparameters + Pareto frontier
"""

from __future__ import annotations

import tempfile
from typing import Any

import axon_quant

# Rust-side HPO module Python bindings
hpo = axon_quant.hpo


def objective_fn(params: dict[str, Any]) -> list[float]:
    """
    HPO objective function.
    Input: A set of hyperparameters (sampled by Optuna based on search_space)
    Output: [sharpe_ratio, -max_drawdown] (multi-objective, both maximized)
    """
    lr = params["learning_rate"]
    gamma = params["gamma"]
    batch_size = params["batch_size"]

    # Reuse step 2's training logic, but with current trial's hyperparameters
    # Simplified: simulate training results with random numbers
    import random
    random.seed(hash((lr, gamma, batch_size)) % 2**32)
    simulated_sharpe = random.uniform(0.5, 2.0) + (lr * 1000)
    simulated_drawdown = random.uniform(0.05, 0.25)

    # Return multi-objective: maximize Sharpe, minimize drawdown (negate for maximization)
    return [simulated_sharpe, -simulated_drawdown]


def main() -> int:
    print("=" * 60)
    print("Step 3: HPO Hyperparameter Search (Multi-Objective)")
    print("=" * 60)

    # -------------------------------------------------
    # 3.1 Define search space
    # -------------------------------------------------
    search_space = {
        "learning_rate": hpo.SearchSpaceDef(param_type="log_uniform", low=1e-5, high=1e-2),
        "gamma": hpo.SearchSpaceDef(param_type="uniform", low=0.95, high=0.999),
        "batch_size": hpo.SearchSpaceDef(param_type="choice", choices=[32, 64, 128, 256]),
    }

    # -------------------------------------------------
    # 3.2 Create OptunaHPO executor
    # -------------------------------------------------
    runner = hpo.OptunaHPO(
        search_space=search_space,
        objective_fn=objective_fn,
        study_name="ppo_momentum_multiobj",
        directions=["maximize", "maximize"],  # Dual objective
        sampler=hpo.SamplerConfig(sampler_type="tpe", n_startup_trials=5, seed=42),
        pruner=hpo.PrunerConfig(pruner_type="median"),
    )

    # -------------------------------------------------
    # 3.3 Execute search
    # -------------------------------------------------
    results = runner.run(n_trials=20, n_jobs=1, timeout_seconds=300)
    print(f"\n[Step 3] Completed {len(results)} trials")

    # -------------------------------------------------
    # 3.4 Get Pareto frontier and hypervolume
    # -------------------------------------------------
    front = runner.get_pareto_front()
    print(f"[Step 3] Pareto frontier points: {len(front)}")
    for p in front[:3]:
        print(f"  trial #{p.trial_id}: sharpe={p.objectives[0]:.3f}, drawdown={-p.objectives[1]:.3f}")

    hv = runner.compute_hypervolume(reference_point=[3.0, 0.0])
    print(f"[Step 3] Hypervolume: {hv:.4f}")

    # -------------------------------------------------
    # 3.5 Output optimal hyperparameters (for next step Walk-Forward / backtest)
    # -------------------------------------------------
    best = runner.get_best_trial()
    if best:
        print(f"[Step 3] Best trial: #{best.trial_id}, params={best.params}")
        # Write optimal hyperparameters to file for downstream steps
        import json
        with open("best_hpo_params.json", "w") as f:
            json.dump(best.params, f, indent=2)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

**Previous step output → Next step input**: `best_hpo_params.json` optimal hyperparameters used to retrain the final model and passed to Walk-Forward validation.

---

## Step 4: Walk-Forward Validation (Purge + Embargo + Deflated Sharpe)

**Input**: Complete dataset + optimal hyperparameters
**Output**: Per-fold OOS (out-of-sample) metrics + aggregated stability analysis

AXON's Walk-Forward module (`axon-walk-forward`) provides leakage detection, Purge, Embargo, and Deflated Sharpe calculation to prevent overfitting and data leakage.

```python
"""
Step 4: Walk-Forward Validation (Purge + Embargo + Deflated Sharpe)
- Split data into multiple train/test folds
- Purge + Embargo clean test set after each fold's training
- Aggregate all fold results, calculate Deflated Sharpe
"""

from __future__ import annotations

import random

import axon_quant

# Rust-side Walk-Forward module Python bindings
wf = axon_quant.walk_forward


def main() -> int:
    print("=" * 60)
    print("Step 4: Walk-Forward Validation")
    print("=" * 60)

    # -------------------------------------------------
    # 4.1 Simulate fold results (real scenario: generated by step 2/3 model on each fold)
    # -------------------------------------------------
    n_folds = 5
    folds = []
    for i in range(n_folds):
        train_idx = list(range(0, 80 + i * 20))
        test_idx = list(range(80 + i * 20, 100 + i * 20))

        # 4.1.1 Leakage detection: ensure no overlap between train and test
        has_leak, pairs = wf.py_detect_leakage(train_idx, test_idx, gap=0)
        assert not has_leak, f"fold {i}: Data leakage detected!"

        # 4.1.2 Purge: Remove train samples overlapping with test labels
        purged_train = wf.py_purge_overlapping_labels(train_idx, test_idx, purge_length=5)
        print(f"  fold {i}: train={len(purged_train)} (after purge), test={len(test_idx)}")

        # 4.1.3 Embargo: Apply embargo to test tail to prevent information leakage
        embargoed_test = wf.py_embargo_indices(test_idx, embargo_pct=0.1, total_len=200)

        # Simulate this fold's test Sharpe (real scenario: from model backtest)
        simulated_sharpe = 1.2 + random.uniform(-0.3, 0.3)
        folds.append({
            "train": purged_train,
            "test": embargoed_test,
            "test_sharpe": simulated_sharpe,
            "test_return": random.uniform(-0.05, 0.15),
        })

    # -------------------------------------------------
    # 4.2 Aggregate OOS results from all folds
    # -------------------------------------------------
    sharpes = [f["test_sharpe"] for f in folds]
    returns = [f["test_return"] for f in folds]
    mean_sharpe = sum(sharpes) / len(sharpes)
    print(f"\n[Step 4] Average OOS Sharpe: {mean_sharpe:.4f}")

    # -------------------------------------------------
    # 4.3 Deflated Sharpe: Correct for multiple testing bias
    # -------------------------------------------------
    dsr = wf.py_deflated_sharpe(observed_sharpe=mean_sharpe, n_trials=20, sharpe_std=0.5)
    print(f"[Step 4] Deflated Sharpe: {dsr:.4f} (original={mean_sharpe:.4f})")
    assert dsr <= mean_sharpe, "Deflated Sharpe should not exceed original Sharpe"

    # -------------------------------------------------
    # 4.4 Stability metrics
    # -------------------------------------------------
    import numpy as np
    sharpe_std = np.std(sharpes, ddof=1)
    sharpe_of_sharpe = mean_sharpe / sharpe_std if sharpe_std > 1e-9 else 0.0
    print(f"[Step 4] Sharpe of Sharpe: {sharpe_of_sharpe:.4f}")

    # Output: Models passing Walk-Forward validation proceed to backtest stage
    print("\n[Step 4] Walk-Forward validation passed, proceeding to backtest stage")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

**Previous step output → Next step input**: Model configuration (hyperparameters + data split strategy) passed to backtesting engine for event-level backtesting.

---

## Step 5: Backtest Validation (BacktestEngine.step())

**Input**: Strategy model (or rule-based strategy) + historical event queue
**Output**: `RunResult` (including PnL, max drawdown, fill statistics, etc.)

AXON's backtesting engine (`axon-backtest`) uses event-driven architecture, supporting L1/L2/L3 matching, market impact model, and step mode.

```python
"""
Step 5: Backtest Validation (BacktestEngine.step())
- Construct event queue (order submission, fill, market data, etc.)
- Use BacktestEngine.run() or step() to advance event by event
- Output: RunResult with PnL, max_drawdown, final_nav, etc.
"""

from __future__ import annotations

# Note: Following code is pseudo-code for Rust API, showing core logic.
# Actual usage requires PyO3 bindings or Rust native calls.

def run_backtest_demo():
    """
    Demonstrate BacktestEngine's core usage pattern.
    Corresponding Rust source: crates/axon-backtest/src/engine.rs
    """
    # -------------------------------------------------
    # 5.1 Construct backtest configuration
    # -------------------------------------------------
    config = {
        "clock": "SimulatedClock",
        "matching_engine": "L1MatchingEngine",
        "impact_model": None,
        "initial_cash": 100_000.0,
    }

    # -------------------------------------------------
    # 5.2 Populate event queue
    # -------------------------------------------------
    # Events in EventQueue are sorted by timestamp, engine consumes in order
    events = []
    # Example: Submit a limit buy order
    events.append({
        "type": "Order",
        "timestamp": 1_000,
        "action": {
            "type": "Submitted",
            "order": {
                "id": 1,
                "symbol": "BTC-USDT",
                "side": "Buy",
                "order_type": {"Limit": {"price": 50_000.0}},
                "quantity": 0.1,
                "time_in_force": "GTC",
            }
        }
    })
    # Example: Submit matching sell order (produces fill)
    events.append({
        "type": "Order",
        "timestamp": 2_000,
        "action": {
            "type": "Submitted",
            "order": {
                "id": 2,
                "symbol": "BTC-USDT",
                "side": "Sell",
                "order_type": {"Limit": {"price": 50_000.0}},
                "quantity": 0.1,
                "time_in_force": "GTC",
            }
        }
    })

    # -------------------------------------------------
    # 5.3 Create engine and run
    # -------------------------------------------------
    # Rust: let mut engine = BacktestEngine::new(config, event_queue);
    #         let result = engine.run();
    print("[Step 5] Creating BacktestEngine and running ...")

    # -------------------------------------------------
    # 5.4 Step mode (for debugging or RL environment interaction)
    # -------------------------------------------------
    # Rust: while let Some(stats) = engine.step() { ... }
    # step() processes one event per call, returns current RunStats snapshot
    print("[Step 5] Step mode: Checking match status event by event")

    # -------------------------------------------------
    # 5.5 Parse results
    # -------------------------------------------------
    result = {
        "events_processed": 2,
        "orders_accepted": 2,
        "fills": 1,
        "total_pnl": -100.0,
        "max_drawdown": 0.0,
        "final_nav": 99_900.0,
    }
    print(f"[Step 5] Backtest result: PnL={result['total_pnl']:.2f}, NAV={result['final_nav']:.2f}")
    return result


if __name__ == "__main__":
    run_backtest_demo()
```

**Previous step output → Next step input**: Model file path passing backtest (e.g., `models/ppo_momentum.zip`) passed to model export module for deployment format conversion.

---

## Step 6: Model Export (ONNX / .pt / .safetensors)

**Input**: Trained model (PyTorch / SB3 format)
**Output**: ONNX / TorchScript `.pt` / `.safetensors` files

AXON's inference layer (`axon-inference`) provides unified `InferenceEngine` trait, supporting ONNX, LibTorch (`.pt`), and Candle backends.

```python
"""
Step 6: Model Export (ONNX / .pt / .safetensors)
- Export SB3/PyTorch model to ONNX or TorchScript
- For Rust-side OnnxBackend / TchBackend to load
"""

from __future__ import annotations

import torch
from pathlib import Path


def export_to_onnx(sb3_model, save_path: Path, input_shape: tuple[int, int, int] = (1, 1, 64)):
    """
    Export SB3 model to ONNX format.
    Corresponding Rust: OnnxBackend::load(path) -> InferenceEngine
    """
    policy = sb3_model.policy
    policy.eval()

    # Construct dummy input (matching Observation.features dimensions)
    dummy_input = torch.randn(input_shape)

    torch.onnx.export(
        policy,
        dummy_input,
        str(save_path),
        export_params=True,
        opset_version=14,
        do_constant_folding=True,
        input_names=["observation"],
        output_names=["action", "value"],
        dynamic_axes={"observation": {0: "batch"}},
    )
    print(f"[Step 6] ONNX model exported: {save_path}")


def export_to_torchscript(sb3_model, save_path: Path, input_shape: tuple[int, int, int] = (1, 1, 64)):
    """
    Export policy network to TorchScript (.pt).
    Corresponding Rust: TchBackend::load(path) -> InferenceEngine
    """
    policy = sb3_model.policy
    policy.eval()
    dummy_input = torch.randn(input_shape)
    traced = torch.jit.trace(policy, dummy_input)
    traced.save(str(save_path))
    print(f"[Step 6] TorchScript model exported: {save_path}")


def main():
    # Assume step 2's trained SB3 model is loaded
    # from stable_baselines3 import PPO
    # model = PPO.load("models/ppo_momentum.zip")

    # Export to ONNX (cross-platform, fast inference)
    # export_to_onnx(model, Path("models/ppo_momentum.onnx"))

    # Export to TorchScript (seamless with Rust tch backend)
    # export_to_torchscript(model, Path("models/ppo_momentum.pt"))

    print("[Step 6] Model export complete, preparing for registry")


if __name__ == "__main__":
    main()
```

**Rust-side inference engine loading example**:

```rust
// Corresponding source: crates/axon-inference/src/backend/onnx.rs
use axon_inference::backend::OnnxBackend;
use axon_inference::engine::InferenceEngine;
use std::path::Path;

fn main() {
    let mut backend = OnnxBackend::new(config);
    // Load ONNX file exported in step 6
    backend.load(Path::new("models/ppo_momentum.onnx")).unwrap();
    // Execute inference
    let action = backend.infer(&observation).unwrap();
    println!("Action: {:?}", action);
}
```

**Previous step output → Next step input**: Exported model file path (e.g., `models/ppo_momentum.onnx`) passed to model registry for version management and stage promotion.

---

## Step 7: Model Registry and Deployment (Registry + Stage Lifecycle)

**Input**: Model artifact file path + metadata (hyperparameters, metrics, Git Commit, etc.)
**Output**: `ModelVersion` record, stage from `STAGING` → `PRODUCTION`

AXON's registry layer (`axon-registry`) provides pure Python model lifecycle management, supporting stage transitions, rollback, and persistence.

```python
"""
Step 7: Model Registry and Deployment (Registry + Stage Lifecycle)
- Register step 6's exported model to ModelRegistry
- Stage flow: STAGING -> PRODUCTION
- Support rollback to previous ARCHIVED version
"""

from __future__ import annotations

import os
import tempfile

import axon_quant

ModelRegistry = axon_quant.registry.ModelRegistry
LocalStorage = axon_quant.registry.LocalStorage
ModelStage = axon_quant.registry.ModelStage


def main() -> int:
    print("=" * 60)
    print("Step 7: Model Registry and Deployment")
    print("=" * 60)

    with tempfile.TemporaryDirectory() as tmp:
        # -------------------------------------------------
        # 7.1 Prepare model artifact (simulate step 6's output)
        # -------------------------------------------------
        model_path = os.path.join(tmp, "ppo_momentum_v1.onnx")
        with open(model_path, "wb") as f:
            f.write(b"ONNX model weights v1 (1024 params)")

        # -------------------------------------------------
        # 7.2 Create storage backend + registry
        # -------------------------------------------------
        storage = LocalStorage(os.path.join(tmp, "models"))
        registry = ModelRegistry(storage)

        # -------------------------------------------------
        # 7.3 Register v1 (defaults to STAGING stage)
        # -------------------------------------------------
        mv1 = registry.register(
            "ppo-momentum",
            model_path,
            metadata={
                "description": "PPO momentum strategy v1",
                "metrics": {"sharpe": 1.5, "max_drawdown": 0.12},
                "hyperparameters": {"learning_rate": 3e-4, "gamma": 0.99},
                "git_commit": "abc1234",
            },
        )
        print(f"\n[Step 7] Register v1: {mv1}")
        print(f"  Initial stage: {mv1.stage.value}")

        # -------------------------------------------------
        # 7.4 Stage transition: STAGING -> PRODUCTION
        # -------------------------------------------------
        mv1_prod = registry.transition_stage(
            "ppo-momentum", mv1.version, ModelStage.PRODUCTION
        )
        print(f"[Step 7] Promoted to Production: {mv1_prod}")

        # -------------------------------------------------
        # 7.5 Query current Production version
        # -------------------------------------------------
        prod = registry.get_production("ppo-momentum")
        print(f"[Step 7] Current Production: {prod}")

        # -------------------------------------------------
        # 7.6 Register v2 (new improved version)
        # -------------------------------------------------
        model_path_v2 = os.path.join(tmp, "ppo_momentum_v2.onnx")
        with open(model_path_v2, "wb") as f:
            f.write(b"ONNX model weights v2 (2048 params)")

        mv2 = registry.register(
            "ppo-momentum",
            model_path_v2,
            metadata={
                "description": "PPO momentum strategy v2",
                "metrics": {"sharpe": 1.8, "max_drawdown": 0.10},
            },
        )
        print(f"\n[Step 7] Register v2: {mv2}")

        # Promote v2 to Production (v1 automatically demotes to ARCHIVED)
        mv2_prod = registry.transition_stage(
            "ppo-momentum", mv2.version, ModelStage.PRODUCTION
        )
        print(f"[Step 7] v2 promoted to Production, v1 automatically demoted to ARCHIVED")

        # -------------------------------------------------
        # 7.7 List all versions
        # -------------------------------------------------
        all_versions = registry.list_versions("ppo-momentum")
        print(f"\n[Step 7] All versions ({len(all_versions)}):")
        for v in all_versions:
            print(f"  - {v}")

        # -------------------------------------------------
        # 7.8 Rollback: from v2 back to v1
        # -------------------------------------------------
        print(f"\n[Step 7] Executing rollback ...")
        rolled = registry.rollback("ppo-momentum")
        print(f"[Step 7] Rollback to Production: {rolled}")

        # -------------------------------------------------
        # 7.9 Download artifact (deploy to inference node)
        # -------------------------------------------------
        dest = os.path.join(tmp, "deployed_model.onnx")
        registry.download_artifact("ppo-momentum", rolled.version, dest)
        print(f"[Step 7] Artifact downloaded to: {dest}")

    print("\n[Step 7] Model registry and deployment pipeline complete")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

---

## Pipeline Data Flow Summary

| Step | Module | Input | Output | Key API |
|------|--------|-------|--------|---------|
| 1 | `axon-data` | Raw market data | `MarketBar[]` + `FeatureConfig[]` | `make_synthetic_market_data` |
| 2 | `axon-rl` | Data + config | `model.zip` | `TradingEnv::new`, `SyncVecEnv::new` |
| 3 | `axon-hpo` | Search space + objective | Optimal hyperparams / Pareto frontier | `OptunaHPO::run` |
| 4 | `axon-walk-forward` | Dataset + hyperparams | OOS metrics + DSR | `py_purge_overlapping_labels`, `py_deflated_sharpe` |
| 5 | `axon-backtest` | Model + event queue | `RunResult` | `BacktestEngine::run`, `step` |
| 6 | `axon-inference` | `model.zip` | `.onnx` / `.pt` | `OnnxBackend::load`, `TchBackend::load` |
| 7 | `axon-registry` | Model file + metadata | `ModelVersion` | `ModelRegistry::register`, `transition_stage` |

---

## Source Code References

- `crates/axon-rl/src/env/trading_env.rs` — `TradingEnv::step()` main loop
- `crates/axon-rl/src/vec_env/sync.rs` — `SyncVecEnv` vectorized environment
- `crates/axon-rl/src/vec_env/async_env.rs` — `AsyncVecEnv` async parallel environment
- `crates/axon-hpo/python/axon_hpo/optuna_runner.py` — `OptunaHPO` wrapper
- `crates/axon-walk-forward/python/axon_walk_forward/evaluation.py` — `aggregate_folds` and DSR
- `crates/axon-backtest/src/engine.rs` — `BacktestEngine::run()` / `step()`
- `crates/axon-inference/src/backend/onnx.rs` — `OnnxBackend`
- `crates/axon-inference/src/backend/tch.rs` — `TchBackend`
- `crates/axon-registry/python/axon_registry/registry.py` — `ModelRegistry`
- `examples/02_rl_training/train_ppo.py` — PPO training complete example
- `examples/03_hpo/hpo_single_objective.py` — HPO basic example
- `examples/08_walk_forward/walk_forward_basic.py` — Walk-Forward basic example
- `examples/05_registry/registry_register_promote.py` — Registry + promote example
