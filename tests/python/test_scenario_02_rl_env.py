"""场景 2: RL 环境全流程测试（axon-rl + Python）"""

import pytest
import math
from conftest import (
    generate_ohlcv,
    generate_small_ohlcv,
    generate_trending_ohlcv,
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
        """2.3 env.reset() 返回 dict"""
        import axon_quant
        bars = generate_small_ohlcv(10)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 10},
            market_data=bars,
        )
        obs = env.reset()
        assert isinstance(obs, dict)
        assert 'features' in obs
        assert 'feature_names' in obs
        assert 'timestamp' in obs
        assert isinstance(obs['features'], list)
        assert len(obs['features']) > 0
        assert not any(math.isnan(x) for x in obs['features'])

    def test_step(self):
        """2.5 env.step(action) 返回 5 元组"""
        import axon_quant
        bars = generate_small_ohlcv(10)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 10},
            market_data=bars,
        )
        env.reset()
        result = env.step([0.5])
        assert isinstance(result, tuple)
        assert len(result) == 5
        obs, reward, done, truncated, info = result
        assert isinstance(obs, dict)
        assert 'features' in obs
        assert isinstance(reward, (int, float))
        assert not math.isnan(reward)
        assert isinstance(done, bool)
        assert isinstance(info, dict)
        assert 'portfolio_value' in info

    def test_full_episode(self):
        """2.6 跑完一个 episode"""
        import axon_quant
        bars = generate_ohlcv(100, seed=123)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 50},
            market_data=bars,
        )
        env.reset()
        total_reward = 0.0
        steps = 0
        for _ in range(100):
            obs, reward, done, truncated, info = env.step([0.0])
            total_reward += reward
            steps += 1
            if done or truncated:
                break

        assert steps > 0, "Episode didn't run any steps"
        assert not math.isnan(total_reward), "Total reward is NaN"

    def test_portfolio_value(self):
        """portfolio_value 属性"""
        import axon_quant
        bars = generate_small_ohlcv(10)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 10},
            market_data=bars,
        )
        assert hasattr(env, 'portfolio_value')
        pv = env.portfolio_value
        assert isinstance(pv, (int, float))
        assert pv > 0

    def test_step_info_fields(self):
        """step info 包含必要字段"""
        import axon_quant
        bars = generate_small_ohlcv(10)
        env = axon_quant.rl.TradingEnv(
            config={"initial_capital": 100_000.0, "max_steps": 10},
            market_data=bars,
        )
        env.reset()
        _, _, _, _, info = env.step([0.5])
        assert 'portfolio_value' in info
        assert 'trades_executed' in info
        assert 'transaction_costs' in info
        assert 'current_step' in info
        assert 'done' in info
