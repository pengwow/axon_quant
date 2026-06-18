#!/usr/bin/env python3
"""
AXON 快速入门示例

演示如何使用 axon_rl 进行基本的强化学习交易：
1. 创建合成市场数据
2. 初始化交易环境
3. 运行随机策略基线
4. 观察环境交互

运行方式：
    cd axon
    python examples/01_getting_started/01_quick_start.py
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

# 设置路径，让 Python 找到 axon_rl 扩展和 _common 工具
_HERE = Path(__file__).resolve().parent.parent
if str(_HERE) not in sys.path:
    sys.path.insert(0, str(_HERE))

import _common  # noqa: E402


def main() -> int:
    print("=" * 60)
    print("AXON 快速入门示例")
    print("=" * 60)

    # 1. 创建合成市场数据（500 根 K 线）
    print("\n[1] 创建合成市场数据...")
    market_data = _common.make_synthetic_market_data(n=500, seed=42)
    print(f"    数据长度: {len(market_data)} 根 K 线")
    print(f"    起始价格: {market_data[0]['close']:.2f}")
    print(f"    结束价格: {market_data[-1]['close']:.2f}")

    # 2. 初始化交易环境
    print("\n[2] 初始化 axon_rl 交易环境...")
    cfg = _common.make_env_config(
        initial_capital=100_000.0,
        max_steps=500,
        seed=42,
        symbol="BTCUSDT",
    )
    env = _common.make_env(config=cfg, market_data=market_data, reward="pnl")
    print(f"    初始资金: {cfg['initial_capital']:,.0f}")
    print(f"    最大步数: {cfg['max_steps']}")
    print(f"    奖励函数: pnl")

    # 3. 运行单个随机 episode
    print("\n[3] 运行随机策略 (1 episode)...")
    t0 = time.perf_counter()
    result = _common.run_random_episode(env, max_steps=500, seed=42)
    elapsed = time.perf_counter() - t0

    print(f"    步数: {result['steps']}")
    print(f"    累计奖励: {result['total_reward']:.4f}")
    print(f"    最终净值: {result['final_value']:,.2f}")
    print(f"    交易次数: {result['trades']}")
    print(f"    耗时: {elapsed:.3f}s")

    # 4. 多 episode 统计
    print("\n[4] 运行 5 个随机 episode 统计...")
    records = []
    for i in range(5):
        r = _common.run_random_episode(env, max_steps=500, seed=i)
        records.append(r)
        print(f"    Episode {i}: reward={r['total_reward']:.4f}, "
              f"final_value={r['final_value']:,.2f}")

    summary = _common.summarize(records)
    print(f"\n    平均奖励: {summary['mean_reward']:.4f}")
    print(f"    平均净值: {summary['mean_final_value']:,.2f}")

    # 5. 验收检查
    print("\n" + "=" * 60)
    completed = sum(1 for r in records if r["steps"] >= 10)
    if completed == 5:
        print("✅ 示例完成! axon_rl 环境运行正常。")
        return 0
    else:
        print(f"❌ 验收失败: 仅 {completed}/5 episodes 完成")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
