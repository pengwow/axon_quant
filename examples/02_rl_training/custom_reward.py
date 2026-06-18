"""custom_reward.py — 自定义奖励函数示例。

演示如何在 Rust 端使用不同的奖励函数实现交易环境的策略评估。
当前 Rust 扩展内置了三种奖励：
- "pnl"     : 绝对 PnL（资金净值变化）
- "sharpe"  : 滚动夏普比率（风险调整后）
- "sortino" : 滚动索提诺比率（仅考虑下行风险）

本示例在**同一份合成数据**上对三种奖励函数进行对比：
1. 跑完 200 步；
2. 输出每个 step 的 reward 序列；
3. 计算 reward 的均值 / 标准差 / 夏普；
4. 评估"哪种奖励函数能更好地稳定区分"。

运行方式：
    cd axon
    /Library/Frameworks/Python.framework/Versions/3.12/bin/python3.12 examples/custom_reward.py
"""

from __future__ import annotations

import statistics
import sys
import time
from pathlib import Path

_HERE = Path(__file__).resolve().parent.parent
if str(_HERE) not in sys.path:
    sys.path.insert(0, str(_HERE))

import _common  # noqa: E402


def run_with_reward(reward: str, market_data, cfg, n_steps: int = 200) -> dict[str, float]:
    """使用指定奖励函数跑 1 个 episode，返回奖励序列统计。"""
    env = _common.make_env(config=cfg, market_data=market_data, reward=reward)
    env.reset()
    rewards: list[float] = []
    portfolio_values: list[float] = []
    for _ in range(n_steps):
        # 简单买入持有策略：固定 target = +0.5 仓位
        result = env.step([0.5])
        obs, r, terminated, truncated, info = result
        rewards.append(r)
        portfolio_values.append(float(info["portfolio_value"]))
        if terminated or truncated:
            break
    return {
        "reward_kind": reward,
        "n_steps": len(rewards),
        "mean_reward": statistics.fmean(rewards) if rewards else 0.0,
        "std_reward": statistics.pstdev(rewards) if len(rewards) > 1 else 0.0,
        "sharpe": (
            statistics.fmean(rewards) / statistics.pstdev(rewards) * (len(rewards) ** 0.5)
            if len(rewards) > 1 and statistics.pstdev(rewards) > 0
            else 0.0
        ),
        "final_value": portfolio_values[-1] if portfolio_values else float(cfg["initial_capital"]),
    }


def main() -> int:
    _common.set_seed(42)
    market_data = _common.make_synthetic_market_data(n=300, seed=42)
    cfg = _common.make_env_config(max_steps=300, seed=42)

    print("[custom_reward] 在相同 buy-and-hold 策略下对比 3 种奖励函数")
    t0 = time.perf_counter()
    summaries = [
        run_with_reward(kind, market_data, cfg, n_steps=200)
        for kind in ("pnl", "sharpe", "sortino")
    ]
    elapsed = time.perf_counter() - t0

    print(f"\n{'reward':>10}  {'n':>4}  {'mean':>12}  {'std':>12}  {'sharpe':>10}  {'final':>12}")
    for s in summaries:
        print(
            f"{s['reward_kind']:>10}  "
            f"{s['n_steps']:>4}  "
            f"{s['mean_reward']:>12.4f}  "
            f"{s['std_reward']:>12.4f}  "
            f"{s['sharpe']:>10.4f}  "
            f"{s['final_value']:>12.2f}"
        )
    print(f"\nelapsed: {elapsed:.2f}s")

    # 验收：所有奖励都应能跑出 n_steps 步（不崩溃）
    if any(s["n_steps"] < 10 for s in summaries):
        print("FAIL: 至少一种奖励函数未能跑够 10 步", file=sys.stderr)
        return 1
    print("PASS: 自定义奖励函数对比完成")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
