"""Ray RLLib 分布式训练器。

支持两种模式：
- **真实模式**：`init_ray=True` 时调用 `ray.init()` 启动真实集群
- **mock 模式**（默认）：`init_ray=False` 时跳过 ray 调用，仅生成 config
  用于 CI / 单元测试 / 无 GPU 环境
"""

from __future__ import annotations

import logging
from typing import Any

from .types import Algorithm, RLLibTrainConfig, RayConfig

logger = logging.getLogger(__name__)


class DistributedTrainer:
    """分布式 RL 训练器，封装 Ray RLLib。"""

    def __init__(
        self,
        ray_config: RayConfig,
        train_config: RLLibTrainConfig,
        init_ray: bool = False,
    ):
        self.ray_config = ray_config
        self.train_config = train_config
        self.init_ray_flag = init_ray
        self._initialized = False
        self._algo: Any = None
        self._iteration_history: list[dict] = []

    @property
    def algorithm(self) -> Any:
        """返回 RLLib algo 实例（mock 模式下为 None）。"""
        return self._algo

    def _ensure_ray_init(self) -> None:
        """确保 Ray 已初始化（mock 模式下跳过）。"""
        if not self.init_ray_flag:
            logger.debug("mock mode: skipping ray.init()")
            return
        if self._initialized:
            return
        # 在 init_ray=True 真实模式下才导入 ray（避免硬依赖）
        import ray  # noqa: PLC0415

        init_kwargs = self.ray_config.to_ray_init_kwargs()
        ray.init(**init_kwargs)
        self._initialized = True
        logger.info("Ray initialized: %s", init_kwargs)

    def build_algo(self) -> Any:
        """构建 RLLib Algorithm 实例（mock 模式返回 None）。"""
        self.train_config.validate()
        self.ray_config.validate()
        if not self.init_ray_flag:
            logger.debug("mock mode: skipping algo build")
            return None

        self._ensure_ray_init()
        if self.train_config.algorithm == Algorithm.PPO.value:
            from ray.rllib.algorithms.ppo import PPOConfig  # noqa: PLC0415

            algo_config = (
                PPOConfig()
                .environment(env=self.train_config.env, env_config=self.train_config.env_config)
                .framework(self.train_config.framework)
                .resources(
                    num_gpus=self.ray_config.num_gpus_per_worker,
                    num_cpus=self.ray_config.num_cpus_per_worker,
                )
                .env_runners(
                    num_env_runners=self.ray_config.num_workers,
                    num_envs_per_worker=self.train_config.num_envs_per_worker,
                    rollout_fragment_length=self.train_config.rollout_fragment_length,
                )
                .training(
                    lr=self.train_config.lr,
                    gamma=self.train_config.gamma,
                    gae_lambda=self.train_config.gae_lambda,
                    clip_param=self.train_config.clip_param,
                    vf_loss_coeff=self.train_config.vf_loss_coeff,
                    entropy_coeff=self.train_config.entropy_coeff,
                    train_batch_size=self.train_config.train_batch_size,
                    sgd_minibatch_size=self.train_config.sgd_minibatch_size,
                    num_sgd_iter=self.train_config.num_sgd_iter,
                )
                .model(self.train_config.model_config)
            )
            self._algo = algo_config.build()
            return self._algo

        if self.train_config.algorithm == Algorithm.SAC.value:
            from ray.rllib.algorithms.sac import SACConfig  # noqa: PLC0415

            algo_config = (
                SACConfig()
                .environment(env=self.train_config.env, env_config=self.train_config.env_config)
                .framework(self.train_config.framework)
                .resources(num_gpus=self.ray_config.num_gpus_per_worker)
                .env_runners(
                    num_env_runners=self.ray_config.num_workers,
                    num_envs_per_worker=self.train_config.num_envs_per_worker,
                )
            )
            self._algo = algo_config.build()
            return self._algo

        raise ValueError(f"Unsupported algorithm: {self.train_config.algorithm}")

    def train(
        self,
        num_iterations: int,
        checkpoint_interval: int = 10,
        checkpoint_dir: str = "checkpoints/",
    ) -> dict[str, Any]:
        """执行分布式训练（mock 模式下生成合成 metrics）。"""
        algo = self.build_algo()
        results = []
        for i in range(num_iterations):
            if algo is not None:
                result = algo.train()
            else:
                # mock：生成合成 metrics
                result = {
                    "env_runners": {
                        "episode_reward_mean": 1.0 + 0.01 * i,
                        "episode_len_mean": 100.0,
                    },
                    "info": {
                        "learner": {
                            "policy_loss": 0.01,
                            "vf_loss": 0.05,
                            "entropy": 0.5,
                        }
                    },
                    "timers": {"training_iteration_time_ms": 1000.0},
                    "iteration": i + 1,
                }
            results.append(result)
            self._iteration_history.append(result)
            if (i + 1) % checkpoint_interval == 0:
                logger.info("iter %d: reward=%.4f", i + 1, self._get_reward(result))

        return {
            "iterations": num_iterations,
            "final_reward": self._get_reward(results[-1]) if results else 0.0,
            "results": results,
        }

    @staticmethod
    def _get_reward(result: dict) -> float:
        return float(result.get("env_runners", {}).get("episode_reward_mean", 0.0))

    def get_history(self) -> list[dict]:
        """返回所有 iteration 的历史记录。"""
        return list(self._iteration_history)
