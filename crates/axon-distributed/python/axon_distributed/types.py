"""AXON 分布式训练类型定义。"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class Algorithm(Enum):
    """支持的算法。"""

    PPO = "PPO"
    SAC = "SAC"
    DQN = "DQN"
    IMPALA = "IMPALA"
    APE_X = "APE_X"


@dataclass
class RayConfig:
    """Ray 集群配置。"""

    num_workers: int = 4
    num_cpus_per_worker: int = 1
    num_gpus_per_worker: float = 0.0
    object_store_memory_gb: float = 2.0
    ray_address: str | None = None  # None=local, "auto"=auto-detect

    def to_ray_init_kwargs(self) -> dict[str, Any]:
        """转换为 ray.init() 参数字典。"""
        kwargs: dict[str, Any] = {
            "num_cpus": self.num_workers * self.num_cpus_per_worker + 2,
            "object_store_memory": int(self.object_store_memory_gb * 1e9),
            "ignore_reinit_error": True,
        }
        if self.num_gpus_per_worker > 0:
            kwargs["num_gpus"] = self.num_workers * self.num_gpus_per_worker
        if self.ray_address:
            kwargs["address"] = self.ray_address
        return kwargs

    def validate(self) -> None:
        if self.num_workers <= 0:
            raise ValueError(f"num_workers ({self.num_workers}) must be > 0")
        if self.num_cpus_per_worker <= 0:
            raise ValueError(f"num_cpus_per_worker ({self.num_cpus_per_worker}) must be > 0")
        if self.num_gpus_per_worker < 0:
            raise ValueError(f"num_gpus_per_worker ({self.num_gpus_per_worker}) must be >= 0")
        if self.object_store_memory_gb <= 0:
            raise ValueError(
                f"object_store_memory_gb ({self.object_store_memory_gb}) must be > 0"
            )


@dataclass
class RLLibTrainConfig:
    """RLLib 训练配置。"""

    algorithm: str = "PPO"
    env: str = "AxonTradingEnv"
    env_config: dict = field(default_factory=dict)
    num_workers: int = 4
    num_envs_per_worker: int = 4
    rollout_fragment_length: int = 200
    train_batch_size: int = 4000
    sgd_minibatch_size: int = 128
    num_sgd_iter: int = 10
    lr: float = 3e-4
    gamma: float = 0.99
    gae_lambda: float = 0.95
    clip_param: float = 0.2
    vf_loss_coeff: float = 0.5
    entropy_coeff: float = 0.01
    framework: str = "torch"
    model_config: dict = field(
        default_factory=lambda: {"fcnet_hiddens": [256, 256], "fcnet_activation": "relu"}
    )

    def validate(self) -> None:
        if self.algorithm not in {a.value for a in Algorithm}:
            raise ValueError(f"algorithm ({self.algorithm}) not supported")
        if self.train_batch_size <= 0:
            raise ValueError(f"train_batch_size ({self.train_batch_size}) must be > 0")
        if self.sgd_minibatch_size <= 0 or self.sgd_minibatch_size > self.train_batch_size:
            raise ValueError(
                f"sgd_minibatch_size ({self.sgd_minibatch_size}) must be in "
                f"(0, train_batch_size={self.train_batch_size}]"
            )

    def to_rllib_config(self, ray_config: RayConfig | None = None) -> dict[str, Any]:
        """转换为 RLLib config 字典。

        Args:
            ray_config: 可选 Ray 集群配置，提供 num_gpus_per_worker 用于注入到 config。
        """
        cfg = {
            "env": self.env,
            "env_config": self.env_config,
            "num_workers": self.num_workers,
            "num_envs_per_worker": self.num_envs_per_worker,
            "rollout_fragment_length": self.rollout_fragment_length,
            "train_batch_size": self.train_batch_size,
            "sgd_minibatch_size": self.sgd_minibatch_size,
            "num_sgd_iter": self.num_sgd_iter,
            "lr": self.lr,
            "gamma": self.gamma,
            "gae_lambda": self.gae_lambda,
            "clip_param": self.clip_param,
            "vf_loss_coeff": self.vf_loss_coeff,
            "entropy_coeff": self.entropy_coeff,
            "model": self.model_config,
            "framework": self.framework,
        }
        if ray_config is not None:
            cfg["num_gpus"] = ray_config.num_gpus_per_worker * ray_config.num_workers
        return cfg

    def _load_default_toml(self) -> None:
        """从默认 TOML 配置加载（仅供测试）。

        路径解析：types.py 位于
        `crates/axon-distributed/python/axon_distributed/types.py`，
        TOML 位于 `crates/axon-distributed/config/default_distributed.toml`，
        即向上三级到达 crate 根目录。
        """
        from pathlib import Path  # noqa: PLC0415

        toml_path = (
            Path(__file__).parent.parent.parent
            / "config"
            / "default_distributed.toml"
        )
        if not toml_path.exists():
            return
        try:
            import tomllib  # Python 3.11+  # noqa: PLC0415
        except ImportError:
            import tomli as tomllib  # type: ignore[no-redef]  # noqa: PLC0415
        with open(toml_path, "rb") as f:
            data = tomllib.load(f)
        cluster = data.get("cluster", {})
        algo = data.get("algorithm", {})
        resources = data.get("resources", {})
        self.algorithm = algo.get("algorithm", self.algorithm)
        self.num_workers = cluster.get("num_workers", self.num_workers)
        self.num_envs_per_worker = resources.get(
            "num_envs_per_worker", self.num_envs_per_worker
        )
        self.rollout_fragment_length = resources.get(
            "rollout_fragment_length", self.rollout_fragment_length
        )
        self.train_batch_size = resources.get(
            "train_batch_size", self.train_batch_size
        )
        self.sgd_minibatch_size = resources.get(
            "sgd_minibatch_size", self.sgd_minibatch_size
        )
        self.num_sgd_iter = resources.get("num_sgd_iter", self.num_sgd_iter)
        self.framework = algo.get("framework", self.framework)
        hparams = algo.get("hparams", {})
        self.lr = hparams.get("lr", self.lr)
        self.gamma = hparams.get("gamma", self.gamma)
        self.gae_lambda = hparams.get("gae_lambda", self.gae_lambda)
        self.clip_param = hparams.get("clip_param", self.clip_param)
        self.vf_loss_coeff = hparams.get("vf_loss_coeff", self.vf_loss_coeff)
        self.entropy_coeff = hparams.get("entropy_coeff", self.entropy_coeff)


@dataclass
class CheckpointConfig:
    """Checkpoint 配置。"""

    checkpoint_dir: str = "checkpoints/"
    checkpoint_interval_s: int = 300
    keep_checkpoints_num: int = 5
    checkpoint_at_end: bool = True
    max_retries: int = 3
