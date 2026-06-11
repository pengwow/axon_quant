"""Parameter Server Actor + DistributedPolicy。

mock 模式下 ParameterServer 退化为本地类，DistributedPolicy 不工作。
"""

from __future__ import annotations

import logging
import pickle
from dataclasses import dataclass, field
from typing import Any

from .actor import RAY_AVAILABLE, _ray_remote

logger = logging.getLogger(__name__)


@dataclass
class ParamServerStats:
    """Parameter Server 统计信息。"""

    version: int
    push_count: int
    pull_count: int


@_ray_remote
class ParameterServer:
    """Parameter Server Actor。"""

    def __init__(self, model_cls: str = "torch.nn.Linear", model_config: dict | None = None):
        self.model_cls = model_cls
        self.model_config = model_config or {}
        self.version: int = 0
        self.gradient_buffer: list = []
        self.push_count: int = 0
        self.pull_count: int = 0

    def get_parameters(self) -> tuple[bytes, int]:
        """拉取当前参数（Worker 调用）。"""
        self.pull_count += 1
        # mock：返回空 dict
        return pickle.dumps({}), self.version

    def push_gradients(self, gradients: bytes, worker_id: int) -> bool:
        """推送梯度（Worker 调用）。"""
        try:
            grad_dict = pickle.loads(gradients)
            self.gradient_buffer.append((worker_id, grad_dict))
        except Exception as e:  # noqa: BLE001
            logger.warning("Failed to unpickle gradients: %s", e)
            return False
        self.version += 1
        self.push_count += 1
        return True

    def get_version(self) -> int:
        return self.version

    def get_stats(self) -> ParamServerStats:
        return ParamServerStats(
            version=self.version,
            push_count=self.push_count,
            pull_count=self.pull_count,
        )


@dataclass
class DistributedPolicy:
    """分布式策略：通过 Parameter Server 同步参数。"""

    param_server_address: str
    worker_id: int = 0
    policy: Any = None
    version: int = 0
    sync_count: int = field(default=0, init=False)

    def sync_parameters(self) -> None:
        """从 Parameter Server 拉取最新参数。"""
        if not RAY_AVAILABLE:
            logger.debug("mock mode: skipping sync_parameters")
            return
        # 真实模式下导入 ray 并调用远程方法
        import ray as _ray  # type: ignore[import-not-found]  # noqa: PLC0415

        server = _ray.get_actor(self.param_server_address)
        params_bytes, version = _ray.get(server.get_parameters.remote())
        if self.policy is not None:
            state_dict = pickle.loads(params_bytes)
            if hasattr(self.policy, "load_state_dict"):
                self.policy.load_state_dict(state_dict)
        self.version = version
        self.sync_count += 1

    def push_update(self, gradients: dict) -> None:
        """推送梯度到 Parameter Server。"""
        if not RAY_AVAILABLE:
            logger.debug("mock mode: skipping push_update")
            return
        # 真实模式下导入 ray 并调用远程方法
        import ray as _ray  # type: ignore[import-not-found]  # noqa: PLC0415

        server = _ray.get_actor(self.param_server_address)
        grad_bytes = pickle.dumps(gradients)
        _ray.get(server.push_gradients.remote(grad_bytes, self.worker_id))
