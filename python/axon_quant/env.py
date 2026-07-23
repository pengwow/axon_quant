"""gym.Env 协议包装 BacktestEngine(0.9.0 D1.1)。

设计目标:
- BacktestEngine 的 Python API → gym.Env,让 SB3 / RLLib 直接训练
- 单 leg observation(spot 或 swap 单一品种)
- action = [-1, 1] 归一化调仓量
- reset(seed=...) 透传 BacktestEngine
"""
from __future__ import annotations

from typing import Any

import gymnasium as gym
import numpy as np
from gymnasium import spaces

from axon_quant._native import backtest as _native_backtest_module
from axon_quant.backtest import InstrumentDict, limit_order, spot_instrument

BacktestEngine = _native_backtest_module.BacktestEngine

# 单 leg observation 维度:32 档(price, qty) * 2 = 64
# 默认与 L3Book top-32 档对齐(实际可配置)
OBS_DIM_SINGLE_LEG: int = 64


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
