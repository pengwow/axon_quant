"""Ray Actor Workers + ActorPool。

设计：
- **延迟导入 ray**：避免硬依赖，未使用时无需安装
- **mock 模式**：当 RAY_AVAILABLE=False 时，EnvironmentWorker 退化为本地类
- **真实模式**：用 @ray.remote 装饰器暴露为 Ray Actor
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any

logger = logging.getLogger(__name__)

# 尝试导入 ray，未安装时回退到 mock 模式
try:
    import ray  # noqa: F401

    RAY_AVAILABLE = True
except ImportError:
    RAY_AVAILABLE = False
    ray = None  # type: ignore[assignment]


# 仅在 RAY_AVAILABLE 时装饰，否则为 no-op
def _ray_remote(cls: type) -> type:
    """条件性 @ray.remote 装饰器（无 ray 时为 no-op）。"""
    if RAY_AVAILABLE:
        return ray.remote(cls)  # type: ignore[union-attr]
    return cls


@dataclass
class WorkerMetrics:
    """单个 Worker 的性能指标。"""

    worker_id: int
    num_envs: int
    avg_reward: float
    total_steps: int = 0


@dataclass
class ActorPool:
    """管理一组 EnvironmentWorker Actors。"""

    num_workers: int
    env_class: str
    env_config: dict
    num_envs_per_worker: int
    observation_space_shape: tuple[int, ...]
    action_space_shape: tuple[int, ...]
    workers: list[Any] = field(default_factory=list, init=False)

    def __post_init__(self) -> None:
        if RAY_AVAILABLE:
            self.workers = [
                EnvironmentWorker.remote(  # type: ignore[attr-defined]
                    worker_id=i,
                    env_class=self.env_class,
                    env_config=self.env_config,
                    num_envs=self.num_envs_per_worker,
                    observation_space_shape=self.observation_space_shape,
                    action_space_shape=self.action_space_shape,
                )
                for i in range(self.num_workers)
            ]
        else:
            # mock 模式：本地实例
            self.workers = [
                EnvironmentWorker(
                    worker_id=i,
                    env_class=self.env_class,
                    env_config=self.env_config,
                    num_envs=self.num_envs_per_worker,
                    observation_space_shape=self.observation_space_shape,
                    action_space_shape=self.action_space_shape,
                )
                for i in range(self.num_workers)
            ]

    def reset_all(self) -> list[dict]:
        """重置所有 Workers。"""
        if RAY_AVAILABLE:
            return ray.get([w.reset.remote() for w in self.workers])  # type: ignore[union-attr]
        return [w.reset() for w in self.workers]

    def step_all(self, actions_list: list) -> list[dict]:
        """并行执行所有 Workers 的 step。"""
        if RAY_AVAILABLE:
            return ray.get(  # type: ignore[union-attr]
                [w.step.remote(actions) for w, actions in zip(self.workers, actions_list)]
            )
        return [w.step(actions) for w, actions in zip(self.workers, actions_list)]

    def get_all_metrics(self) -> list[WorkerMetrics]:
        """获取所有 Worker 的性能指标。"""
        if RAY_AVAILABLE:
            return ray.get([w.get_metrics.remote() for w in self.workers])  # type: ignore[union-attr]
        return [w.get_metrics() for w in self.workers]


@_ray_remote
class EnvironmentWorker:
    """远程环境 Actor Worker。"""

    def __init__(
        self,
        worker_id: int,
        env_class: str,
        env_config: dict,
        num_envs: int,
        observation_space_shape: tuple,
        action_space_shape: tuple,
    ):
        self.worker_id = worker_id
        self.num_envs = num_envs
        self.env_class = env_class
        self.env_config = env_config
        self.observation_space_shape = observation_space_shape
        self.action_space_shape = action_space_shape

        # 状态
        self.observations: list = [None] * num_envs
        self.dones: list = [True] * num_envs
        self.rewards: list = [0.0] * num_envs
        self.episode_rewards: list = [0.0] * num_envs
        self.total_steps: int = 0

    def reset(self) -> dict:
        """重置所有环境，返回初始观测（mock 模式返回零向量）。"""
        if not RAY_AVAILABLE:
            self.observations = [self._mock_observation() for _ in range(self.num_envs)]
        else:
            try:
                # 真实模式下尝试导入 axon_env
                from axon_env import AxonTradingEnv  # type: ignore  # noqa: PLC0415

                self.observations = [
                    AxonTradingEnv(self.env_config).reset() for _ in range(self.num_envs)
                ]
            except ImportError:
                logger.warning("axon_env not available, using mock observations")
                self.observations = [self._mock_observation() for _ in range(self.num_envs)]
        self.dones = [False] * self.num_envs
        self.episode_rewards = [0.0] * self.num_envs
        return {
            "worker_id": self.worker_id,
            "observations": self.observations,
        }

    def step(self, actions: list) -> dict:
        """执行动作，返回经验 batch。"""
        rewards = []
        for i in range(self.num_envs):
            if self.dones[i]:
                # 自动重置已完成的环境
                self.observations[i] = (
                    self._mock_observation() if not RAY_AVAILABLE else self.observations[i]
                )
                self.dones[i] = False
                self.episode_rewards[i] = 0.0
                rewards.append(0.0)
            else:
                # mock：返回常数奖励
                r = 0.01
                self.episode_rewards[i] += r
                rewards.append(r)
            self.total_steps += 1
        return {
            "worker_id": self.worker_id,
            "rewards": rewards,
            "episode_rewards": list(self.episode_rewards),
        }

    def get_metrics(self) -> WorkerMetrics:
        """获取 Worker 级别的性能指标。"""
        avg_reward = (
            sum(self.episode_rewards) / len(self.episode_rewards)
            if self.episode_rewards
            else 0.0
        )
        return WorkerMetrics(
            worker_id=self.worker_id,
            num_envs=self.num_envs,
            avg_reward=avg_reward,
            total_steps=self.total_steps,
        )

    def _mock_observation(self) -> list:
        """生成 mock 观测（零向量）。"""
        return [0.0] * self.observation_space_shape[-1]
