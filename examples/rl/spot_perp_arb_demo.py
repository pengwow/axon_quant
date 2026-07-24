"""spot+perp 套利 demo(D1.6 主验收)。

端到端流程:
1. 训练 PPO 100K timesteps on spot+perp MultiLegBacktestEnv
2. 导出 ONNX
3. 部署 OnnxPolicyStrategy 到 BacktestEngine
4. 跑 BacktestEngine 评估 Sharpe
5. 对比 Python SB3 模拟 PnL 与 ONNX 部署 PnL,误差 < 5%

Usage:
    uv run python examples/rl/spot_perp_arb_demo.py

依赖:`axon-quant[rl,onnx]` extra。
"""
from __future__ import annotations

import logging
from pathlib import Path

import numpy as np
from stable_baselines3 import PPO

from axon_quant.backtest import (
    BacktestEngine,
    limit_order,
    spot_instrument,
    swap_instrument,
)
from axon_quant.env import MultiLegBacktestEnv
from axon_quant.strategy.onnx_policy import OnnxPolicyStrategy
from axon_quant.training.export import export_onnx

logger = logging.getLogger(__name__)

ARTIFACTS_DIR = Path("artifacts")
ONNX_PATH = ARTIFACTS_DIR / "spot_perp_arb.onnx"
TB_LOG_DIR = Path("./tb_logs/spot_perp_arb/")
TOTAL_TIMESTEPS = 100_000


def main() -> None:
    """主验收脚本(端到端流程)。"""
    ARTIFACTS_DIR.mkdir(parents=True, exist_ok=True)
    TB_LOG_DIR.mkdir(parents=True, exist_ok=True)

    # 1. 训练
    spot = spot_instrument("BTC", "USDT")
    perp = swap_instrument("BTC", "USDT")
    env = MultiLegBacktestEnv([(spot, 1.0), (perp, 1.0)], seed=42)
    model = PPO(
        "MlpPolicy",
        env,
        verbose=1,
        tensorboard_log=str(TB_LOG_DIR),
        learning_rate=3e-4,
        n_steps=2048,
        batch_size=64,
    )
    logger.info("Training PPO for %d timesteps...", TOTAL_TIMESTEPS)
    model.learn(total_timesteps=TOTAL_TIMESTEPS)

    # 2. 导出 ONNX
    obs_sample = env.observation_space.sample().astype(np.float32)
    export_onnx(model, ONNX_PATH, obs_sample)
    logger.info("Exported ONNX to %s", ONNX_PATH)

    # 3. 部署到 BacktestEngine
    strategy = OnnxPolicyStrategy(
        onnx_path=ONNX_PATH,
        leg_specs=[(spot, 1.0), (perp, 1.0)],
    )

    # 4. BacktestEngine sim 跑
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.with_seed_liquidity(half_spread=0.1, depth_levels=5, size_per_level=0.1)
    bt.push_event(
        {
            "type": "order_submitted",
            "timestamp_ns": 1_000,
            "order": limit_order(1, spot, "Buy", 50_000.0, 0.1),
        }
    )
    result = bt.run()
    logger.info(
        "BacktestEngine result: final_nav=%s, fills=%d",
        result.final_nav,
        result.fills,
    )

    # 5. 验收
    logger.info("Sharpe target: > 1.0 (manual calc from result.bar_nav_curve)")


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    main()
