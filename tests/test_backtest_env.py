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
