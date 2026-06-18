"""random_agent.py — 随机策略基线示例。

不依赖任何第三方 RL 库（`stable-baselines3` / `gymnasium`），仅用
`axon_rl` Rust 扩展和 `examples/_common.py` 即可运行。适合作为：
1. 端到端冒烟测试：验证 Rust 扩展 + Python 接口 + 行情数据流通畅。
2. 性能基线：后续训练的策略应明显优于随机策略。
3. CI 入口：可放入 CI 中，无需 GPU / 训练依赖。

运行方式：
    cd axon
    /Library/Frameworks/Python.framework/Versions/3.12/bin/python3.12 examples/random_agent.py
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

# ── 路径设置：让 `import _common` 走 examples 目录 ──
_HERE = Path(__file__).resolve().parent.parent
if str(_HERE) not in sys.path:
    sys.path.insert(0, str(_HERE))

import _common  # noqa: E402


def main() -> int:
    _common.set_seed(42)

    # 1. 准备数据 + 环境
    market_data = _common.make_synthetic_market_data(n=500, seed=42)
    cfg = _common.make_env_config(max_steps=500, seed=42)
    env = _common.make_env(config=cfg, market_data=market_data, reward="pnl")

    # 2. 多 episode 随机策略
    n_episodes = 5
    max_steps = 500
    print(f"[random_agent] 运行 {n_episodes} 个随机 episode，每个最多 {max_steps} 步")
    t0 = time.perf_counter()
    records = [
        _common.run_random_episode(env, max_steps=max_steps, seed=i)
        for i in range(n_episodes)
    ]
    elapsed = time.perf_counter() - t0

    # 3. 输出统计
    summary = _common.summarize(records)
    print("\n=== 随机策略基线 ===")
    print(f"  episodes        : {int(summary['n'])}")
    print(f"  mean_reward     : {summary['mean_reward']:.4f}")
    print(f"  mean_steps      : {summary['mean_steps']:.1f}")
    print(f"  mean_final_value: {summary['mean_final_value']:.2f}")
    print(f"  elapsed         : {elapsed:.2f}s")

    # 4. 验收：所有 episode 都应能跑完而不崩溃
    completed = sum(1 for r in records if r["steps"] >= 5)
    if completed < n_episodes:
        print(
            f"FAIL: 仅 {completed}/{n_episodes} episodes 跑过 5 步",
            file=sys.stderr,
        )
        return 1
    print("PASS: 随机策略运行正常")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
