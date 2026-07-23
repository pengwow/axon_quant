"""gym.Env 协议包装 BacktestEngine(0.9.0 D1.1)。

设计目标:
- BacktestEngine 的 Python API → gym.Env,让 SB3 / RLLib 直接训练
- 单 leg observation(spot 或 swap 单一品种)
- action = [-1, 1] 归一化调仓量
- reset(seed=...) 透传 BacktestEngine
"""
from __future__ import annotations

from typing import Any, NamedTuple

import gymnasium as gym
import numpy as np
from gymnasium import spaces

from axon_quant._native import backtest as _native_backtest_module
from axon_quant.backtest import InstrumentDict, limit_order, spot_instrument

BacktestEngine = _native_backtest_module.BacktestEngine

# 单 leg observation 维度:32 档(price, qty) * 2 = 64
# 默认与 L3Book top-32 档对齐(实际可配置)
OBS_DIM_SINGLE_LEG: int = 64


class LegSpec(NamedTuple):
    """单 leg 规格:(instrument, target_qty_scale)。

    target_qty_scale: 调仓量归一化系数(action [-1, 1] * scale = 实际 qty)
    """
    instrument: InstrumentDict
    target_qty_scale: float = 1.0


class BacktestEnv(gym.Env):
    """BacktestEngine 的 gym.Env 协议包装(单 leg observation)。

    observation_space: Box(OBS_DIM_SINGLE_LEG,)float32
    action_space: Box(1,)float32 ∈ [-1, 1]
    """

    metadata = {"render_modes": ["human"]}

    def __init__(
        self,
        instrument: InstrumentDict,
        initial_cash: float = 100_000.0,
        seed: int | None = None,
    ) -> None:
        super().__init__()
        self.instrument = instrument
        self.initial_cash = initial_cash
        self.engine = BacktestEngine(initial_cash=initial_cash)
        self._seed = seed
        self.observation_space = spaces.Box(
            low=-np.inf, high=np.inf,
            shape=(OBS_DIM_SINGLE_LEG,),
            dtype=np.float32,
        )
        self.action_space = spaces.Box(
            low=-1.0, high=1.0, shape=(1,),
            dtype=np.float32,
        )

    def reset(
        self,
        *,
        seed: int | None = None,
        options: dict[str, Any] | None = None,
    ) -> tuple[np.ndarray, dict[str, Any]]:
        """重置 env + BacktestEngine。

        Args:
            seed: 透传 BacktestEngine.with_seed(seed)(0.9.0 新增 builder)
            options: gym 标准参数,0.9.0 暂未使用

        Returns:
            (obs, info) — obs 形状 = observation_space.shape
        """
        super().reset(seed=seed)
        if seed is not None:
            self.engine = BacktestEngine(initial_cash=self.initial_cash).with_seed(seed)
        else:
            self.engine = BacktestEngine(initial_cash=self.initial_cash)
        self._prev_nav: float = self.initial_cash
        obs = self._build_obs()
        info: dict[str, Any] = {"nav": self.initial_cash, "position": 0.0}
        return obs, info

    def step(
        self, action: np.ndarray,
    ) -> tuple[np.ndarray, float, bool, bool, dict[str, Any]]:
        """推进 1 bar,执行 action,返回 (obs, reward, terminated, truncated, info)。

        action[0] ∈ [-1, 1] 归一化调仓量:
            action = 1.0 → set_target_position(instrument, +1.0)
            action = -1.0 → set_target_position(instrument, -1.0)
            action = 0.0 → set_target_position(instrument, 0.0)
        """
        target_qty = float(action[0])  # 已经在 [-1, 1]
        self.engine.set_target_position(self.instrument, target_qty)
        # 触发 1 个空 bar(价格用初始 NAV 代替,实际 demo 中是动态的)
        self.engine.begin_bar(self._prev_nav, self.instrument)
        result = self.engine.run()
        nav = result.final_nav
        reward = (nav - self._prev_nav) / self.initial_cash  # 相对收益
        self._prev_nav = nav
        obs = self._build_obs()
        terminated = bool(result.events_processed == 0)  # 事件耗尽
        truncated = False
        info: dict[str, Any] = {
            "nav": nav,
            "position": 0.0,  # 0.9.0 简化:实际应读 engine state
            "fills": result.fills,
        }
        return obs, float(reward), terminated, truncated, info

    def _build_obs(self) -> np.ndarray:
        """构造单 leg observation(简化版:用 L3Book top-32 档)。

        0.9.0 简化:返回 zeros(OBS_DIM_SINGLE_LEG,)(TODO:读真实 L3Book)
        实际生产化时,读 BacktestEngine 当前 L3Book + position + cash。
        """
        # 占位:实际 D1.1 完整实施时,调 self.engine.book_snapshot(self.instrument)
        return np.zeros(self.observation_space.shape, dtype=np.float32)


class MultiLegBacktestEnv(BacktestEnv):
    """多 leg observation / action 拼接(2-3 leg 同步)。

    observation: concat(各 leg OBS_DIM_SINGLE_LEG bytes + 各 leg position + cash)
    action: shape (n_legs,), 每 leg 一个 [-1, 1] 调仓量
    """

    def __init__(
        self,
        legs: list[LegSpec],
        initial_cash: float = 100_000.0,
        seed: int | None = None,
    ) -> None:
        if len(legs) < 2:
            raise ValueError(f"MultiLegBacktestEnv requires ≥ 2 legs, got {len(legs)}")
        # 兼容 tuple 入参:[(instrument, scale), ...] → [LegSpec, ...]
        self.legs = [
            leg if isinstance(leg, LegSpec) else LegSpec(leg[0], leg[1])
            for leg in legs
        ]
        # primary 是第一个 leg(向后兼容 BacktestEnv 接口)
        super().__init__(self.legs[0].instrument, initial_cash, seed)
        n_legs = len(self.legs)
        # obs shape: 各 leg OBS_DIM_SINGLE_LEG bytes + 各 leg 2 position 字段 + 1 cash
        obs_dim = OBS_DIM_SINGLE_LEG * n_legs + 2 * n_legs + 1
        self.observation_space = spaces.Box(
            low=-np.inf, high=np.inf,
            shape=(obs_dim,),
            dtype=np.float32,
        )
        self.action_space = spaces.Box(
            low=-1.0, high=1.0, shape=(n_legs,),
            dtype=np.float32,
        )

    def _build_obs(self) -> np.ndarray:
        """多 leg obs 拼接。

        layout: [leg0_obs(64) | leg1_obs(64) | ... | leg0_pos(2) | leg1_pos(2) | ... | cash(1)]
        0.9.0 简化:order book 维度读 self.engine 内部 L3Book(TODO 完整实施)。
        """
        n_legs = len(self.legs)
        # 各 leg order book obs(简化:全 0,占位 64 bytes)
        obs_parts = [np.zeros(OBS_DIM_SINGLE_LEG, dtype=np.float32) for _ in range(n_legs)]
        # 各 leg position(简化:全 0,占位 2 bytes)
        pos_parts = [np.zeros(2, dtype=np.float32) for _ in range(n_legs)]
        # cash(占位 1 byte)
        cash_part = np.zeros(1, dtype=np.float32)
        return np.concatenate(obs_parts + pos_parts + [cash_part])

    def step(
        self, action: np.ndarray,
    ) -> tuple[np.ndarray, float, bool, bool, dict[str, Any]]:
        """多 leg 同步调仓。"""
        if action.shape != (len(self.legs),):
            raise ValueError(
                f"action shape {action.shape} != ({len(self.legs)},)",
            )
        # 各 leg set_target_position
        for i, leg in enumerate(self.legs):
            target_qty = float(action[i]) * leg.target_qty_scale
            self.engine.set_target_position(leg.instrument, target_qty)
        # 同步推进(用 primary instrument 价格驱动)
        self.engine.begin_bar(self._prev_nav, self.legs[0].instrument)
        result = self.engine.run()
        nav = result.final_nav
        reward = (nav - self._prev_nav) / self.initial_cash
        self._prev_nav = nav
        obs = self._build_obs()
        terminated = bool(result.events_processed == 0)
        truncated = False
        info: dict[str, Any] = {
            "nav": nav,
            "fills": result.fills,
        }
        return obs, float(reward), terminated, truncated, info
