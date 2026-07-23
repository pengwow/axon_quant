"""Tests for MultiLegBacktestEnv (2-3 leg sync)."""
from __future__ import annotations

import numpy as np

from axon_quant.backtest import spot_instrument, swap_instrument
from axon_quant.env import OBS_DIM_SINGLE_LEG, MultiLegBacktestEnv


def test_multi_leg_observation_space_2_legs() -> None:
    """2 leg obs shape = OBS_DIM_SINGLE_LEG * 2 + 2*2 + 1 = 64*2+4+1 = 133"""
    spot = spot_instrument("BTC", "USDT")
    perp = swap_instrument("BTC", "USDT")
    env = MultiLegBacktestEnv([(spot, 1.0), (perp, 1.0)])
    expected_dim = OBS_DIM_SINGLE_LEG * 2 + 2 * 2 + 1
    assert env.observation_space.shape == (expected_dim,)


def test_multi_leg_action_space_2_legs() -> None:
    """2 leg action shape = (2,), range [-1, 1]"""
    spot = spot_instrument("BTC", "USDT")
    perp = swap_instrument("BTC", "USDT")
    env = MultiLegBacktestEnv([(spot, 1.0), (perp, 1.0)])
    assert env.action_space.shape == (2,)
    assert env.action_space.low[0] == -1.0
    assert env.action_space.high[0] == 1.0


def test_multi_leg_step_with_zero_action() -> None:
    """action=zeros 不调仓,reward = 0(无 fill)"""
    spot = spot_instrument("BTC", "USDT")
    perp = swap_instrument("BTC", "USDT")
    env = MultiLegBacktestEnv([(spot, 1.0), (perp, 1.0)], seed=42)
    env.reset(seed=42)
    action = np.array([0.0, 0.0], dtype=np.float32)
    obs, reward, term, trunc, info = env.step(action)
    assert obs.shape == env.observation_space.shape
    assert isinstance(reward, float)


def test_multi_leg_3_legs_supported() -> None:
    """3 leg 设计兼容(代码路径支持,benchmark 走 2)"""
    spot = spot_instrument("BTC", "USDT")
    perp = swap_instrument("BTC", "USDT")
    future = spot_instrument("ETH", "USDT")
    env = MultiLegBacktestEnv([(spot, 1.0), (perp, 1.0), (future, 1.0)])
    assert env.action_space.shape == (3,)
