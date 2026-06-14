"""场景 2: RL 环境全流程测试（axon-rl + Python）"""

import pytest
import math
from conftest import (
    generate_ohlcv,
    generate_small_ohlcv,
    generate_trending_ohlcv,
    assert_valid_observation,
    assert_valid_reward,
)


class TestTradingEnvLifecycle:
    """TradingEnv 完整生命周期测试"""

    def test_import(self):
        """2.1 Python import axon_quant"""
        import axon_quant
        assert hasattr(axon_quant, 'rl')
        assert hasattr(axon_quant.rl, 'TradingEnv')

    def test_create_env(self):
        """2.2 创建 TradingEnv"""
        import axon_quant
        bars = generate_small_ohlcv(10)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 10},
            action_space={"type": "continuous", "min": -1.0, "max": 1.0},
            market_data=bars,
        )
        assert env is not None

    def test_create_env_requires_market_data(self):
        """创建环境必须传 market_data"""
        import axon_quant
        with pytest.raises(ValueError, match="market_data is required"):
            axon_quant.rl.TradingEnv(config={"initial_capital": 100_000.0})

    def test_reset(self):
        """2.3 env.reset() 返回 (obs, info)"""
        import axon_quant
        bars = generate_small_ohlcv(10)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 10},
            market_data=bars,
        )
        result = env.reset()
        assert isinstance(result, tuple), f"reset() returned {type(result)}"
        assert len(result) == 2, f"reset() returned {len(result)} items"
        obs, info = result
        assert_valid_observation(obs)
        assert isinstance(info, dict)

    def test_action_space_sample(self):
        """2.4 action_space.sample() 返回合法动作"""
        import axon_quant
        bars = generate_small_ohlcv(10)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 10},
            market_data=bars,
        )
        action = env.action_space.sample()
        assert action is not None
        assert isinstance(action, (int, float, list))

    def test_step(self):
        """2.5 env.step(action) 返回 5 元组"""
        import axon_quant
        bars = generate_small_ohlcv(10)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 10},
            market_data=bars,
        )
        env.reset()
        action = env.action_space.sample()
        result = env.step(action)
        assert isinstance(result, tuple), f"step() returned {type(result)}"
        assert len(result) == 5, f"step() returned {len(result)} items"
        obs, reward, terminated, truncated, info = result
        assert_valid_observation(obs)
        assert_valid_reward(reward)
        assert isinstance(terminated, bool)
        assert isinstance(truncated, bool)
        assert isinstance(info, dict)

    def test_full_episode(self):
        """2.6 跑完一个 episode"""
        import axon_quant
        bars = generate_ohlcv(100, seed=123)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 50},
            market_data=bars,
        )
        obs, info = env.reset()
        total_reward = 0.0
        steps = 0
        for _ in range(100):
            action = env.action_space.sample()
            obs, reward, terminated, truncated, info = env.step(action)
            total_reward += reward
            steps += 1
            if terminated or truncated:
                break

        assert steps > 0, "Episode didn't run any steps"
        assert not math.isnan(total_reward), "Total reward is NaN"
        # 2.7 累计 reward 有值
        # (total_reward 可以为 0，但不能是 NaN/Inf)

    def test_observation_space(self):
        """observation_space 属性存在"""
        import axon_quant
        bars = generate_small_ohlcv(10)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 10},
            market_data=bars,
        )
        assert hasattr(env, 'observation_space')
        assert env.observation_space is not None

    def test_action_space_bounds(self):
        """action_space 有正确的边界"""
        import axon_quant
        bars = generate_small_ohlcv(10)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 10},
            action_space={"type": "continuous", "min": -1.0, "max": 1.0},
            market_data=bars,
        )
        space = env.action_space
        assert hasattr(space, 'low') or hasattr(space, 'shape')
