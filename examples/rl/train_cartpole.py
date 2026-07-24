"""CartPole 烟雾测试(RLLib 路径)。

验证 `axon_distributed.DistributedTrainer` + Ray RLLib PPO 集成 OK。
CartPole 是经典 gym 环境,5K timesteps 应当收敛到 reward > 475。

Usage:
    uv run python examples/rl/train_cartpole.py

Acceptance: Episode reward mean > 475 within 10 iterations.

依赖:`axon-distributed` + `ray[rllib]` + `torch`。
未安装时,`init_ray=True` 会报 `ModuleNotFoundError`,这是预期行为;
不传 `init_ray`(默认 `False`,mock 模式)可用于 CI 烟雾测试。
"""
from __future__ import annotations

import logging

# axon_distributed 的 `__init__.py` 0.0.1 版只导出 `__version__`,
# 这里直接 import 子模块的符号(避免依赖 __init__ re-export)。
from axon_distributed.ray_trainer import DistributedTrainer
from axon_distributed.types import RayConfig, RLLibTrainConfig

logger = logging.getLogger(__name__)

CARTPOLE_REWARD_THRESHOLD = 475
CARTPOLE_MAX_ITERATIONS = 10


def train_cartpole(init_ray: bool = True) -> bool:
    """训练 CartPole,返回是否在 10 iter 内收敛。

    Args:
        init_ray: True 真实模式(需 ray + RLLib 环境),
            False mock 模式(用于 CI / 单元测试烟雾)。

    Returns:
        bool: True 表示 10 iter 内 `episode_reward_mean > 475`,
            或真实模式下命中收敛阈值。
            mock 模式始终返回 False(无 algo)。
    """
    # 注:axon_distributed 0.0.1 `RLLibTrainConfig.validate` 把 `self.algorithm`
    # 与 `Algorithm.value` 集合比较时,期望字符串而非 enum 实例;
    # 这里传 `algorithm="PPO"` 字符串(避免 enum 误用)。
    trainer = DistributedTrainer(
        ray_config=RayConfig(num_workers=1, num_cpus_per_worker=1),
        train_config=RLLibTrainConfig(
            algorithm="PPO",
            env="CartPole-v1",
            framework="torch",
            train_batch_size=4000,
            sgd_minibatch_size=128,
            lr=1e-4,
        ),
        init_ray=init_ray,
    )
    algo = trainer.build_algo()
    if algo is None:
        logger.warning("algo is None (mock mode?) — skipping real training")
        return False

    for i in range(CARTPOLE_MAX_ITERATIONS):
        result = algo.train()
        reward_mean = result.get("episode_reward_mean", 0.0)
        logger.info(f"iter {i}: episode_reward_mean = {reward_mean:.1f}")
        if reward_mean > CARTPOLE_REWARD_THRESHOLD:
            logger.info(f"CartPole converged at iter {i}")
            return True

    logger.warning(f"CartPole did not converge in {CARTPOLE_MAX_ITERATIONS} iterations")
    return False


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    converged = train_cartpole()
    print(f"Converged: {converged}")
