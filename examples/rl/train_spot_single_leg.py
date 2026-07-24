"""Spot 单 leg PPO 50K 收敛 demo(SB3 路径)。

Usage:
    uv run python examples/rl/train_spot_single_leg.py

完整 50K 训练需要 `axon-quant[rl]` extra(gymnasium / stable-baselines3 /
torch)。脚本本身是 standalone 的,缺依赖会报清晰错误。

Acceptance: 模型保存到 `artifacts/spot_single_leg_ppo.zip`,可加载 + predict。
"""
from __future__ import annotations

import logging
from pathlib import Path

from stable_baselines3 import PPO

from axon_quant.backtest import spot_instrument
from axon_quant.env import BacktestEnv

logger = logging.getLogger(__name__)

ARTIFACTS_DIR = Path("artifacts")
MODEL_PATH = ARTIFACTS_DIR / "spot_single_leg_ppo.zip"
TB_LOG_DIR = Path("./tb_logs/spot_single_leg/")
TOTAL_TIMESTEPS = 50_000


def train_spot_single_leg(total_timesteps: int = TOTAL_TIMESTEPS) -> Path:
    """训练 spot 单 leg PPO,返回 model 路径。

    Args:
        total_timesteps: 训练步数(默认 50K;测试可传 50 走 smoke)。
    """
    ARTIFACTS_DIR.mkdir(parents=True, exist_ok=True)
    TB_LOG_DIR.mkdir(parents=True, exist_ok=True)

    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot, seed=42)

    model = PPO(
        "MlpPolicy",
        env,
        verbose=1,
        tensorboard_log=str(TB_LOG_DIR),
        learning_rate=3e-4,
        n_steps=2048,
        batch_size=64,
    )
    model.learn(total_timesteps=total_timesteps)
    model.save(str(MODEL_PATH))
    logger.info(f"Model saved to {MODEL_PATH}")
    return MODEL_PATH


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    path = train_spot_single_leg()
    print(f"Trained model: {path}")
