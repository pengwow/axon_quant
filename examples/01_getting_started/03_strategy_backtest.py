#!/usr/bin/env python3
"""
AXON 策略回测示例

演示如何使用 axon_rl 进行策略回测：
1. 定义多种交易策略（动量、均值回归等）
2. 在 axon_rl 环境中运行回测
3. 对比策略性能
4. 生成回测报告

运行方式：
    cd axon
    python examples/01_getting_started/03_strategy_backtest.py
"""

from __future__ import annotations

import random
import sys
import time
from pathlib import Path

# 设置路径
_HERE = Path(__file__).resolve().parent.parent
if str(_HERE) not in sys.path:
    sys.path.insert(0, str(_HERE))

import _common  # noqa: E402


class MomentumStrategy:
    """动量策略：根据近期收益决定仓位。"""

    def __init__(self, lookback: int = 10, threshold: float = 0.01):
        self.lookback = lookback
        self.threshold = threshold
        self.prices = []

    def __call__(self, obs_dict: dict) -> list[float]:
        close = obs_dict.get("features", [0.0])[0] if "features" in obs_dict else 0.0
        self.prices.append(close)

        if len(self.prices) < self.lookback:
            return [0.0]  # 数据不足，空仓

        # 计算动量（避免除零）
        base_price = self.prices[-self.lookback]
        if base_price <= 0:
            return [0.0]
        recent_return = (self.prices[-1] / base_price) - 1

        if recent_return > self.threshold:
            return [0.8]  # 做多
        elif recent_return < -self.threshold:
            return [-0.8]  # 做空
        else:
            return [0.0]  # 空仓


class MeanReversionStrategy:
    """均值回归策略：价格偏离均线时反向操作。"""

    def __init__(self, window: int = 20, threshold: float = 0.02):
        self.window = window
        self.threshold = threshold
        self.prices = []

    def __call__(self, obs_dict: dict) -> list[float]:
        close = obs_dict.get("features", [0.0])[0] if "features" in obs_dict else 0.0
        self.prices.append(close)

        if len(self.prices) < self.window:
            return [0.0]

        # 计算偏离度（避免除零）
        mean_price = sum(self.prices[-self.window:]) / self.window
        if mean_price <= 0:
            return [0.0]
        deviation = (close - mean_price) / mean_price

        if deviation > self.threshold:
            return [-0.6]  # 价格过高，做空
        elif deviation < -self.threshold:
            return [0.6]  # 价格过低，做多
        else:
            return [0.0]


class RSIStrategy:
    """RSI 策略：基于相对强弱指标交易。"""

    def __init__(self, period: int = 14, overbought: float = 70, oversold: float = 30):
        self.period = period
        self.overbought = overbought
        self.oversold = oversold
        self.prices = []

    def __call__(self, obs_dict: dict) -> list[float]:
        close = obs_dict.get("features", [0.0])[0] if "features" in obs_dict else 0.0
        self.prices.append(close)

        if len(self.prices) < self.period + 1:
            return [0.0]

        # 计算 RSI
        gains = []
        losses = []
        for i in range(-self.period, 0):
            change = self.prices[i] - self.prices[i - 1]
            if change > 0:
                gains.append(change)
                losses.append(0)
            else:
                gains.append(0)
                losses.append(-change)

        avg_gain = sum(gains) / self.period
        avg_loss = sum(losses) / self.period

        if avg_loss == 0:
            rsi = 100.0
        else:
            rs = avg_gain / avg_loss
            rsi = 100 - (100 / (1 + rs))

        if rsi < self.oversold:
            return [0.7]  # 超卖，做多
        elif rsi > self.overbought:
            return [-0.7]  # 超买，做空
        else:
            return [0.0]


def run_strategy_backtest(env, strategy_fn, max_steps: int = 500) -> dict:
    """运行策略回测。"""
    obs = env.reset()
    if isinstance(obs, tuple):
        obs = obs[0]

    total_reward = 0.0
    steps = 0
    portfolio_values = []
    actions_taken = []
    done = False

    while not done and steps < max_steps:
        # 获取观测信息
        if isinstance(obs, dict):
            obs_dict = obs
        else:
            # 如果是数组，转换为 dict 格式
            obs_dict = {"features": list(obs) if hasattr(obs, "__len__") else [obs]}

        # 策略决策
        action = strategy_fn(obs_dict)

        # 执行动作
        result = env.step(action)
        if len(result) == 5:
            obs, reward, terminated, truncated, info = result
            done = bool(terminated) or bool(truncated)
        else:
            obs, reward, done, info = result

        total_reward += float(reward)
        steps += 1

        # 记录数据
        pv = float(info.get("portfolio_value", 0.0)) if isinstance(info, dict) else 0.0
        portfolio_values.append(pv)
        actions_taken.append(action[0] if isinstance(action, list) else action)

    return {
        "total_reward": total_reward,
        "steps": steps,
        "final_value": portfolio_values[-1] if portfolio_values else 0.0,
        "portfolio_values": portfolio_values,
        "actions": actions_taken,
    }


def calculate_performance(portfolio_values: list[float]) -> dict:
    """计算策略性能指标。"""
    if len(portfolio_values) < 2:
        return {}

    import numpy as np  # noqa: PLC0415

    pv = np.array(portfolio_values)
    total_return = pv[-1] / pv[0] - 1

    # 日收益率
    returns = np.diff(pv) / pv[:-1]
    returns = returns[np.isfinite(returns)]

    if len(returns) == 0:
        return {"total_return": total_return}

    # 夏普比率
    sharpe = np.mean(returns) / np.std(returns) * np.sqrt(252) if np.std(returns) > 0 else 0.0

    # 最大回撤
    peak = np.maximum.accumulate(pv)
    drawdown = (peak - pv) / peak
    max_drawdown = np.max(drawdown)

    # 胜率
    win_rate = np.sum(returns > 0) / len(returns)

    return {
        "total_return": total_return,
        "sharpe_ratio": sharpe,
        "max_drawdown": max_drawdown,
        "win_rate": win_rate,
    }


def main() -> int:
    print("=" * 60)
    print("AXON 策略回测示例")
    print("=" * 60)

    # 1. 准备环境
    print("\n[1] 准备回测环境...")
    market_data = _common.make_synthetic_market_data(n=500, seed=42)
    cfg = _common.make_env_config(initial_capital=100_000.0, max_steps=500, seed=42)
    env = _common.make_env(config=cfg, market_data=market_data, reward="pnl")
    print(f"    初始资金: {cfg['initial_capital']:,.0f}")
    print(f"    回测长度: {len(market_data)} 根 K 线")

    # 2. 定义策略
    strategies = {
        "随机基线": lambda obs: [random.uniform(-1, 1)],
        "动量策略 (10日)": MomentumStrategy(lookback=10, threshold=0.01),
        "均值回归 (20日)": MeanReversionStrategy(window=20, threshold=0.02),
        "RSI 策略 (14日)": RSIStrategy(period=14, overbought=70, oversold=30),
        "固定做多 (0.8)": lambda obs: [0.8],
        "固定做空 (-0.8)": lambda obs: [-0.8],
    }

    # 3. 运行回测
    print("\n[2] 运行策略回测...")
    results = {}
    for name, strategy in strategies.items():
        t0 = time.perf_counter()
        result = run_strategy_backtest(env, strategy, max_steps=500)
        elapsed = time.perf_counter() - t0
        result["elapsed"] = elapsed
        results[name] = result
        print(f"    {name}: reward={result['total_reward']:.4f}, "
              f"final_value={result['final_value']:,.2f}, "
              f"steps={result['steps']}, time={elapsed:.3f}s")

    # 4. 计算性能指标
    print("\n[3] 策略性能对比...")
    print(f"{'策略':<20} {'收益率':<12} {'夏普比率':<10} {'最大回撤':<10} {'胜率':<8}")
    print("-" * 60)

    performance = {}
    for name, result in results.items():
        if result["portfolio_values"]:
            perf = calculate_performance(result["portfolio_values"])
            performance[name] = perf
            print(f"{name:<20} "
                  f"{perf.get('total_return', 0):<12.2%} "
                  f"{perf.get('sharpe_ratio', 0):<10.2f} "
                  f"{perf.get('max_drawdown', 0):<10.2%} "
                  f"{perf.get('win_rate', 0):<8.2%}")
        else:
            print(f"{name:<20} {'N/A':<12} {'N/A':<10} {'N/A':<10} {'N/A':<8}")

    # 5. 策略排名
    print("\n[4] 策略排名（按夏普比率）...")
    ranked = sorted(performance.items(),
                    key=lambda x: x[1].get("sharpe_ratio", -999),
                    reverse=True)
    for i, (name, perf) in enumerate(ranked, 1):
        sharpe = perf.get("sharpe_ratio", 0)
        total_ret = perf.get("total_return", 0)
        print(f"    {i}. {name}: 夏普={sharpe:.2f}, 收益={total_ret:+.2%}")

    # 6. 最佳策略详情
    if ranked:
        best_name, best_perf = ranked[0]
        print(f"\n[5] 最佳策略: {best_name}")
        print(f"    总收益率: {best_perf.get('total_return', 0):.2%}")
        print(f"    夏普比率: {best_perf.get('sharpe_ratio', 0):.2f}")
        print(f"    最大回撤: {best_perf.get('max_drawdown', 0):.2%}")
        print(f"    胜率: {best_perf.get('win_rate', 0):.2%}")

    print("\n" + "=" * 60)
    print("✅ 策略回测示例完成!")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
