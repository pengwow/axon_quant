"""spot+perp HPO sweep 示例(D1.5b)。

Usage:
    # 串行 2 trial smoke
    uv run python examples/rl/hpo_spot_perp_demo.py --n-trials 2 --total-timesteps 50 --n-jobs 1

    # 8-CPU 并发 100 trial
    uv run python examples/rl/hpo_spot_perp_demo.py --n-trials 100 --total-timesteps 50000 --n-jobs 8

依赖:`axon-quant[rl]` extra(SB3 + torch)。
"""
from __future__ import annotations

import argparse
import logging
from typing import Any

from stable_baselines3 import PPO

from axon_quant.backtest import spot_instrument, swap_instrument
from axon_quant.env import MultiLegBacktestEnv
from axon_quant.training.hpo_sweeper import RLHPOSweeper, make_tb_log_dir

logger = logging.getLogger(__name__)


def objective(params: dict[str, Any]) -> list[float]:
    """单 trial:训练 spot+perp N timesteps,返回 Sharpe(占位 1.0)。

    注意:0.9.0 简化版只返回占位 reward,真实 Sharpe 计算在 D1.6 主验收脚本中实现。
    """
    spot = spot_instrument("BTC", "USDT")
    perp = swap_instrument("BTC", "USDT")
    env = MultiLegBacktestEnv([(spot, 1.0), (perp, 1.0)], seed=42)
    model = PPO("MlpPolicy", env, verbose=0, **params)
    # trial 0 用 TB(其余 trial 也用独立目录),便于在 tensorboard --logdir ./tb_logs/ 观察
    tb_dir = make_tb_log_dir(trial_id=getattr(model, "_trial_id", 0))
    model.learn(total_timesteps=50_000, tb_log_name=tb_dir)
    return [1.0]  # 占位 reward


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--n-trials", type=int, default=2)
    parser.add_argument("--total-timesteps", type=int, default=50)
    parser.add_argument("--n-jobs", type=int, default=1)
    parser.add_argument(
        "--storage", type=str, default="sqlite:///optuna_rl_hpo.db"
    )
    args = parser.parse_args()

    sweeper = RLHPOSweeper(
        study_name="spot_perp_rl_hpo",
        n_trials=args.n_trials,
        storage=args.storage,
        n_jobs=args.n_jobs,
    )
    best = sweeper.sweep(objective_fn=objective)
    print(f"Best config: {best}")


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    main()
