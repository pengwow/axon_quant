"""AXON 测试 fixtures — 共享测试数据和辅助函数"""

import random
import math


def generate_ohlcv(n: int = 200, seed: int = 42, base_price: float = 50000.0) -> list[dict]:
    """生成模拟 OHLCV K 线数据
    
    Args:
        n: K 线数量
        seed: 随机种子（可复现）
        base_price: 基准价格
    
    Returns:
        list of dict with keys: timestamp, open, high, low, close, volume
    """
    rng = random.Random(seed)
    bars = []
    price = base_price
    for i in range(n):
        change = rng.gauss(0, 100)
        open_p = price
        close_p = price + change
        high_p = max(open_p, close_p) + abs(rng.gauss(0, 30))
        low_p = min(open_p, close_p) - abs(rng.gauss(0, 30))
        volume = abs(rng.gauss(0, 10)) + 0.1
        bars.append({
            "timestamp": 1_000_000 + i * 60_000,
            "open": open_p,
            "high": high_p,
            "low": low_p,
            "close": close_p,
            "volume": volume,
        })
        price = close_p
    return bars


def generate_small_ohlcv(n: int = 10) -> list[dict]:
    """生成少量确定性 K 线（用于快速测试）"""
    prices = [100.0, 102.0, 101.0, 103.0, 105.0, 104.0, 106.0, 108.0, 107.0, 110.0]
    bars = []
    for i in range(min(n, len(prices))):
        p = prices[i]
        bars.append({
            "timestamp": 1_000_000 + i * 60_000,
            "open": p - 0.5,
            "high": p + 1.0,
            "low": p - 1.0,
            "close": p,
            "volume": 1.0 + i * 0.1,
        })
    return bars


def generate_trending_ohlcv(n: int = 100, trend: float = 1.0) -> list[dict]:
    """生成有趋势的 K 线数据
    
    Args:
        n: 数量
        trend: 每步趋势增量（正=上涨，负=下跌）
    """
    bars = []
    price = 100.0
    for i in range(n):
        noise = random.gauss(0, 0.5)
        open_p = price
        close_p = price + trend + noise
        high_p = max(open_p, close_p) + abs(random.gauss(0, 0.2))
        low_p = min(open_p, close_p) - abs(random.gauss(0, 0.2))
        bars.append({
            "timestamp": 1_000_000 + i * 60_000,
            "open": open_p,
            "high": high_p,
            "low": low_p,
            "close": close_p,
            "volume": random.uniform(0.5, 5.0),
        })
        price = close_p
    return bars


def assert_valid_observation(obs, expected_features: int = 2):
    """验证观测值格式正确"""
    assert obs is not None, "observation is None"
    assert hasattr(obs, 'shape'), f"observation has no shape: {type(obs)}"
    assert obs.shape[-1] == expected_features, f"expected {expected_features} features, got {obs.shape[-1]}"
    assert not math.isnan(obs.sum()), "observation contains NaN"
    assert not math.isinf(obs.sum()), "observation contains Inf"


def assert_valid_reward(reward):
    """验证 reward 格式正确"""
    assert isinstance(reward, (int, float)), f"reward type: {type(reward)}"
    assert not math.isnan(reward), "reward is NaN"
    assert not math.isinf(reward), "reward is Inf"
