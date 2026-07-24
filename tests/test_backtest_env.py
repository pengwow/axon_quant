"""Tests for BacktestEnv (gym.Env wrapper around BacktestEngine)."""
from __future__ import annotations

import numpy as np
import pytest

from axon_quant.backtest import spot_instrument
from axon_quant.env import OBS_DIM_SINGLE_LEG, BacktestEnv


def test_backtest_env_observation_space_shape() -> None:
    """observation_space is Box with shape (OBS_DIM_SINGLE_LEG,)."""
    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot)
    assert env.observation_space.shape == (OBS_DIM_SINGLE_LEG,)
    assert env.observation_space.dtype == np.float32


def test_backtest_env_action_space_shape() -> None:
    """action_space is Box with shape (1,) and range [-1, 1]."""
    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot)
    assert env.action_space.shape == (1,)
    assert env.action_space.low[0] == -1.0
    assert env.action_space.high[0] == 1.0


def test_backtest_env_metadata() -> None:
    """metadata.render_modes contains 'human'."""
    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot)
    assert "human" in env.metadata["render_modes"]


def test_backtest_env_reset_returns_obs_and_info() -> None:
    """reset() returns (obs, info) with obs in observation_space."""
    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot, seed=42)
    obs, info = env.reset(seed=42)
    assert obs.shape == (OBS_DIM_SINGLE_LEG,)
    assert obs.dtype == np.float32
    assert isinstance(info, dict)


def test_backtest_env_step_returns_correct_tuple() -> None:
    """step(action) returns (obs, reward, terminated, truncated, info)."""
    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot, seed=42)
    env.reset(seed=42)
    action = np.array([0.0], dtype=np.float32)
    result = env.step(action)
    assert len(result) == 5
    obs, reward, terminated, truncated, info = result
    assert obs.shape == (OBS_DIM_SINGLE_LEG,)
    assert isinstance(reward, float)
    assert isinstance(terminated, bool)
    assert isinstance(truncated, bool)
    assert isinstance(info, dict)


def test_backtest_env_action_zero_holds_position() -> None:
    """action=0 不调整仓位,info['position'] 应保持 0(初始状态)。"""
    spot = spot_instrument("BTC", "USDT")
    env = BacktestEnv(spot, seed=42)
    env.reset(seed=42)
    _, _, _, _, info = env.step(np.array([0.0], dtype=np.float32))
    assert info.get("position", 0.0) == 0.0
