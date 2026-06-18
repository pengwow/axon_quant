"""_vec_env.py — `axon_rl.TradingEnv` 的 Gymnasium 包装与向量化封装。

为 `stable-baselines3` 等需要 `gymnasium.Env` / `gymnasium.vector.VectorEnv`
接口的 RL 库提供：

- `AxonTradingEnv`：
    单个 `axon_rl.TradingEnv` 的 `gymnasium.Env` 包装（仅当 gymnasium
    可用）。自动把 `features` 张量拼成 `Box(obs_dim,)`、把 `info` dict
    暴露给回调。
- `make_vec_env`：
    工厂函数，返回 `stable_baselines3.DummyVecEnv`（推荐）或轻量级 fallback
    `FallbackVecEnv`（无 gym/sb3 时的"伪向量化"包装）。

设计原则：
- 不在 Python 端重新实现 `TradingEnv` 的 step 逻辑；所有计算仍在
  Rust 扩展中。
- 包装层只做：(1) 数据格式转换 `dict ↔ numpy`、`Tuple ↔ gym.spaces`；
  (2) `info` dict 透传；(3) `gymnasium` API 兼容。
- 当 `gymnasium` 不可用时仍能让脚本"跑通"—— fallback 包装的 API
  形状与 `DummyVecEnv` 接近，足以支撑冒烟测试与简单 loop。
"""

from __future__ import annotations

import sys
from typing import Any, Callable, Optional

import _common  # noqa: E402


# ──────────────────────────────────────────────
# gymnasium 可用性探测
# ──────────────────────────────────────────────


def _try_import_gymnasium() -> tuple[bool, Any]:
    """尝试 import `gymnasium` 与 `gymnasium.spaces`，返回 (ok, module)。"""
    try:
        import gymnasium  # noqa: PLC0415
        import gymnasium.spaces  # noqa: F401, PLC0415

        return True, gymnasium
    except ImportError:
        return False, None


def _try_import_sb3() -> tuple[bool, Any]:
    """尝试 import `stable_baselines3`，返回 (ok, module)。"""
    try:
        import stable_baselines3  # noqa: PLC0415
        from stable_baselines3.common.vec_env import DummyVecEnv  # noqa: PLC0415

        return True, DummyVecEnv
    except ImportError:
        return False, None


def _try_import_async_vec_env() -> tuple[bool, Any]:
    """尝试 import `gymnasium.vector.AsyncVectorEnv`，返回 (ok, module)。"""
    try:
        from gymnasium.vector import AsyncVectorEnv  # noqa: PLC0415

        return True, AsyncVectorEnv
    except ImportError:
        return False, None


# ──────────────────────────────────────────────
# 单环境包装
# ──────────────────────────────────────────────


def _build_gym_env_class():
    """构造 `AxonTradingEnv`（继承 `gymnasium.Env` 当且仅当 gymnasium 可用）。"""
    gym_ok, gym_module = _try_import_gymnasium()
    if not gym_ok:
        return _AxonTradingEnvFallback

    import gymnasium as gym  # noqa: PLC0415
    import numpy as np  # noqa: PLC0415

    class AxonTradingEnv(gym.Env):  # type: ignore[misc, valid-type]
        """`axon_rl.TradingEnv` 的 Gymnasium 兼容包装（继承 `gymnasium.Env`）。

        把 Rust 端 `step` 返回的 `(obs_dict, reward, terminated, truncated, info)`
        转换为 Gymnasium 风格：
            - `obs`: `numpy.ndarray`（features 展平 + 标量时间戳）
            - `reward`: float
            - `terminated` / `truncated` / `info`: dict

        实现要点：
        - `action_space` / `observation_space` 从 Rust 端属性推断（连续 [-1, 1]
          与 Box 形状）。`stable-baselines3` 主要靠这两个属性做 sanity check。
        - 自动重置：当 `terminated or truncated`，下次 `step()` 先 `reset()`，
          保证不返回 `(obs, _, True, _, _)` 后立即崩溃。
        """

        metadata = {"render.modes": ["human"]}

        def __init__(self, env) -> None:
            super().__init__()
            self._env = env
            self._needs_reset = True
            self._obs_dim: Optional[int] = None
            self._action_dim: int = 1
            self._action_low: float = -1.0
            self._action_high: float = 1.0
            self._init_spaces()

        def _init_spaces(self) -> None:
            # reset 一次拿到真实 obs 维度
            self._env.reset()
            self._needs_reset = False
            try:
                result = self._env.step([0.0])
                obs_dict, _r, _t, _tr, _info = result
                self._obs_dim = len(obs_dict["features"]) + 1  # +1 for timestamp
            except Exception:
                self._obs_dim = 3
                self._env.reset()
            self._needs_reset = True
            # 定义 spaces（必须在 reset 后才能确定 obs_dim）
            self.action_space = gym.spaces.Box(
                low=np.full(self._action_dim, self._action_low, dtype=np.float32),
                high=np.full(self._action_dim, self._action_high, dtype=np.float32),
                dtype=np.float32,
            )
            self.observation_space = gym.spaces.Box(
                low=-np.inf,
                high=np.inf,
                shape=(self._obs_dim,),
                dtype=np.float32,
            )

        def reset(self, *, seed: int | None = None, options: dict | None = None):
            super().reset(seed=seed)
            self._env.reset()
            self._needs_reset = False
            return self._zero_obs(), {}

        def step(self, action):
            if self._needs_reset:
                self._env.reset()
                self._needs_reset = False
            # 兼容 sb3 的 ndarray 输入
            if hasattr(action, "tolist"):
                action = action.tolist()
            if isinstance(action, (int, float)):
                action = [float(action)]
            action = [float(a) for a in action]
            result = self._env.step(action)
            obs_dict, reward, terminated, truncated, info = result
            if not isinstance(info, dict):
                info = {"raw": info}
            if terminated or truncated:
                self._needs_reset = True
            return (
                self._get_obs_from_dict(obs_dict),
                float(reward),
                bool(terminated),
                bool(truncated),
                info,
            )

        def render(self):
            return self._env.render() if hasattr(self._env, "render") else None

        def close(self):
            if hasattr(self._env, "close"):
                try:
                    self._env.close()
                except Exception:
                    pass

        def _get_obs_from_dict(self, obs_dict: dict) -> np.ndarray:
            feats = list(obs_dict.get("features", []))
            ts = float(obs_dict.get("timestamp", 0))
            return np.array(feats + [ts], dtype=np.float32)

        def _zero_obs(self) -> np.ndarray:
            """reset 后的初始 obs（features 均为 0，timestamp=0）。"""
            return np.zeros(self._obs_dim, dtype=np.float32)

    return AxonTradingEnv


class _AxonTradingEnvFallback:
    """无 gym 时的最小"鸭子类型"包装（不支持 sb3）。"""

    def __init__(self, env) -> None:
        self._env = env
        self._needs_reset = True
        self.observation_space = _FallbackBox(low=-1e9, high=1e9, shape=(3,))
        self.action_space = _FallbackBox(low=-1.0, high=1.0, shape=(1,))

    def reset(self, *, seed: int | None = None, options: dict | None = None):
        self._env.reset()
        self._needs_reset = False
        return self._obs_dict_to_list({}), {}

    def step(self, action):
        if self._needs_reset:
            self._env.reset()
            self._needs_reset = False
        if hasattr(action, "tolist"):
            action = action.tolist()
        if isinstance(action, (int, float)):
            action = [float(action)]
        result = self._env.step(list(action))
        obs_dict, reward, terminated, truncated, info = result
        if terminated or truncated:
            self._needs_reset = True
        return self._obs_dict_to_list(obs_dict), float(reward), bool(terminated), bool(truncated), info

    def close(self):
        if hasattr(self._env, "close"):
            try:
                self._env.close()
            except Exception:
                pass

    @staticmethod
    def _obs_dict_to_list(obs_dict: dict) -> list[float]:
        feats = list(obs_dict.get("features", []))
        ts = float(obs_dict.get("timestamp", 0))
        return feats + [ts]


# 动态选基类
AxonTradingEnv = _build_gym_env_class()


class _FallbackBox:
    """无 gym/numpy 时的最小 Box 替代品（仅用于 .shape / .low / .high 读取）。"""

    def __init__(self, low: float, high: float, shape: tuple[int, ...]) -> None:
        self.low = low
        self.high = high
        self.shape = shape
        self.dtype = float

    def __repr__(self) -> str:
        return f"_FallbackBox(low={self.low}, high={self.high}, shape={self.shape})"


# ──────────────────────────────────────────────
# 轻量级向量化包装（用于无 gym / sb3 场景）
# ──────────────────────────────────────────────


class _FallbackVecEnv:
    """最简"伪向量化"包装：单线程顺序执行 N 个 `axon_rl.TradingEnv` 实例。

    不使用线程、不会自动 reset done 的环境。仅用于"无 gym 也能跑通
    示例"的冒烟场景；正式训练请使用 `stable_baselines3` 的 `DummyVecEnv` /
    `SubprocVecEnv`，或在 Rust 端使用 `SyncVecEnv` / `AsyncVecEnv`。
    """

    num_envs: int

    def __init__(self, env_fns: list[Callable[[], Any]], num_envs: int | None = None) -> None:
        self.num_envs = num_envs or len(env_fns)
        self.envs = [fn() for fn in env_fns]

    def reset(self) -> list[Any]:
        return [self._obs(e.reset()) for e in self.envs]

    def step(
        self, actions: list[Any]
    ) -> tuple[list[Any], list[float], list[bool], list[dict]]:
        obs_list, rew_list, done_list, info_list = [], [], [], []
        for env, action in zip(self.envs, actions):
            if hasattr(action, "tolist"):
                action = action.tolist()
            result = env.step(action if isinstance(action, list) else [float(action)])
            o, r, t, tr, info = result
            obs_list.append(self._obs(o))
            rew_list.append(float(r))
            done_list.append(bool(t) or bool(tr))
            info_list.append(info if isinstance(info, dict) else {"raw": info})
        return obs_list, rew_list, done_list, info_list

    def close(self) -> None:
        for env in self.envs:
            if hasattr(env, "close"):
                try:
                    env.close()
                except Exception:
                    pass

    @staticmethod
    def _obs(o: Any) -> Any:
        try:
            import numpy as np  # noqa: PLC0415

            if isinstance(o, dict):
                feats = list(o.get("features", []))
                ts = float(o.get("timestamp", 0))
                return np.array(feats + [ts], dtype=np.float32)
        except ImportError:
            pass
        return o


def make_vec_env(
    env_fn: Callable[[], Any],
    n_envs: int = 1,
    use_stable_baselines3: bool = True,
    use_async: bool = False,
) -> Any:
    """构造向量化环境：优先使用 `stable_baselines3.DummyVecEnv`，否则 fallback。

    Args:
        env_fn: 工厂函数 `() -> axon_rl.TradingEnv`（或已包装的 `AxonTradingEnv`）
        n_envs: 并行环境数
        use_stable_baselines3: True 时尝试 sb3；False 时强制使用 fallback
        use_async: 当 n_envs >= 4 时自动使用 AsyncVectorEnv（多进程并行）

    Returns:
        支持 `reset() -> list[obs]` 与
        `step(actions) -> (obs, rewards, dones, infos)` 的对象
    """
    # 当 n_envs >= 4 且 use_async=True 时，尝试使用 AsyncVectorEnv
    if use_async and n_envs >= 4:
        async_ok, AsyncVectorEnv = _try_import_async_vec_env()
        if async_ok:
            return AsyncVectorEnv([env_fn for _ in range(n_envs)])

    if use_stable_baselines3:
        sb3_ok, DummyVecEnv = _try_import_sb3()
        if sb3_ok:
            return DummyVecEnv([env_fn for _ in range(n_envs)])
    return _FallbackVecEnv([env_fn for _ in range(n_envs)], num_envs=n_envs)


__all__ = [
    "AxonTradingEnv",
    "make_vec_env",
]
