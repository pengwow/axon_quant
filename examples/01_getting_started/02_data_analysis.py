#!/usr/bin/env python3
"""
AXON 数据分析示例

演示如何使用 axon_rl 进行交易数据分析：
1. 运行多种策略（随机、固定仓位）
2. 收集交易数据
3. 计算性能指标（收益率、夏普比率、最大回撤）
4. 策略对比分析

运行方式：
    cd axon
    python examples/01_getting_started/02_data_analysis.py
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


def run_fixed_action(env, action: float, max_steps: int = 500, seed: int = 42) -> dict:
    """运行固定仓位策略。"""
    random.seed(seed)
    env.reset()
    total_reward = 0.0
    steps = 0
    portfolio_values = []
    done = False

    while not done and steps < max_steps:
        result = env.step([action])
        obs_dict, reward, terminated, truncated, info = result
        total_reward += reward
        steps += 1
        portfolio_values.append(float(info.get("portfolio_value", 0.0)))
        done = bool(terminated) or bool(truncated)

    return {
        "total_reward": total_reward,
        "steps": steps,
        "final_value": portfolio_values[-1] if portfolio_values else 0.0,
        "portfolio_values": portfolio_values,
        "trades": int(info.get("trades_executed", 0)),
    }


def calculate_metrics(portfolio_values: list[float], risk_free_rate: float = 0.0) -> dict:
    """计算性能指标。"""
    if len(portfolio_values) < 2:
        return {}

    # 计算收益率序列
    returns = []
    for i in range(1, len(portfolio_values)):
        if portfolio_values[i - 1] > 0:
            returns.append(portfolio_values[i] / portfolio_values[i - 1] - 1)

    if not returns:
        return {}

    import numpy as np  # noqa: PLC0415

    returns = np.array(returns)
    total_return = portfolio_values[-1] / portfolio_values[0] - 1

    # 夏普比率 (年化，假设 252 个交易日)
    mean_return = np.mean(returns)
    std_return = np.std(returns)
    sharpe_ratio = (mean_return - risk_free_rate) / std_return * np.sqrt(252) if std_return > 0 else 0.0

    # 最大回撤
    peak = np.maximum.accumulate(portfolio_values)
    drawdown = (peak - portfolio_values) / peak
    max_drawdown = np.max(drawdown)

    # 胜率
    winning_days = np.sum(returns > 0)
    win_rate = winning_days / len(returns) if len(returns) > 0 else 0.0

    return {
        "total_return": total_return,
        "sharpe_ratio": sharpe_ratio,
        "max_drawdown": max_drawdown,
        "win_rate": win_rate,
        "volatility": std_return * np.sqrt(252),
    }


def main() -> int:
    print("=" * 60)
    print("AXON 数据分析示例")
    print("=" * 60)

    # 1. 准备环境
    print("\n[1] 准备交易环境...")
    market_data = _common.make_synthetic_market_data(n=500, seed=42)
    cfg = _common.make_env_config(initial_capital=100_000.0, max_steps=500, seed=42)
    env = _common.make_env(config=cfg, market_data=market_data, reward="pnl")
    print(f"    初始资金: {cfg['initial_capital']:,.0f}")
    print(f"    数据长度: {len(market_data)} 根 K 线")

    # 2. 定义策略
    strategies = {
        "随机策略": {"type": "random"},
        "全仓做多 (1.0)": {"type": "fixed", "action": 1.0},
        "半仓做多 (0.5)": {"type": "fixed", "action": 0.5},
        "空仓 (0.0)": {"type": "fixed", "action": 0.0},
        "半仓做空 (-0.5)": {"type": "fixed", "action": -0.5},
        "全仓做空 (-1.0)": {"type": "fixed", "action": -1.0},
    }

    # 3. 运行策略
    print("\n[2] 运行策略分析...")
    results = {}
    for name, config in strategies.items():
        t0 = time.perf_counter()
        if config["type"] == "random":
            records = []
            for i in range(5):
                r = _common.run_random_episode(env, max_steps=500, seed=i)
                records.append(r)
            summary = _common.summarize(records)
            result = {
                "total_reward": summary["mean_reward"],
                "final_value": summary["mean_final_value"],
                "steps": summary["mean_steps"],
            }
        else:
            result = run_fixed_action(env, config["action"], max_steps=500, seed=42)

        elapsed = time.perf_counter() - t0
        result["elapsed"] = elapsed
        results[name] = result
        print(f"    {name}: reward={result['total_reward']:.4f}, "
              f"final_value={result['final_value']:,.2f}, "
              f"time={elapsed:.3f}s")

    # 4. 计算性能指标
    print("\n[3] 性能指标对比...")
    print(f"{'策略':<20} {'收益率':<12} {'夏普比率':<10} {'最大回撤':<10}")
    print("-" * 55)

    for name, result in results.items():
        if "portfolio_values" in result and len(result["portfolio_values"]) > 1:
            metrics = calculate_metrics(result["portfolio_values"])
            print(f"{name:<20} "
                  f"{metrics.get('total_return', 0):<12.2%} "
                  f"{metrics.get('sharpe_ratio', 0):<10.2f} "
                  f"{metrics.get('max_drawdown', 0):<10.2%}")
        else:
            # 随机策略没有 portfolio_values 序列
            total_return = result['final_value'] / 100_000.0 - 1
            print(f"{name:<20} {total_return:<12.2%} {'N/A':<10} {'N/A':<10}")

    # 5. 最佳策略
    print("\n[4] 策略排名（按最终净值）...")
    ranked = sorted(results.items(), key=lambda x: x[1]["final_value"], reverse=True)
    for i, (name, result) in enumerate(ranked, 1):
        total_return = result['final_value'] / 100_000.0 - 1
        print(f"    {i}. {name}: {result['final_value']:,.2f} ({total_return:+.2%})")

    print("\n" + "=" * 60)
    print("✅ 数据分析示例完成!")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
