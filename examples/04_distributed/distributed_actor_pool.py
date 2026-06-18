"""distributed_actor_pool.py — 分布式基础功能示例。

使用 `axon_quant.distributed` 模块。

运行方式：
    cd axon
    .venv/bin/python examples/04_distributed/distributed_actor_pool.py
"""

from __future__ import annotations

import json

import axon_quant  # noqa: E402
py_serialize_metrics = axon_quant.distributed.py_serialize_metrics


def main() -> int:
    print("=" * 60)
    print("分布式基础功能示例")
    print("=" * 60)

    # 模拟多个 worker 的指标
    print("\n[1] 序列化多个 worker 的指标")
    workers_data = []
    for worker_id in range(3):
        metrics_json = py_serialize_metrics(
            step=100 * (worker_id + 1),
            reward=0.5 + 0.1 * worker_id,
            policy_loss=0.01 * (worker_id + 1),
            value_loss=0.02 * (worker_id + 1),
            entropy=0.1,
            fps=800.0 + 200 * worker_id,
        )
        metrics = json.loads(metrics_json)
        workers_data.append(metrics)
        print(f"  worker {worker_id}: step={metrics['step']}, "
              f"reward={metrics['episode_reward_mean']:.2f}")

    # 汇总
    total_steps = sum(m["step"] for m in workers_data)
    avg_reward = sum(m["episode_reward_mean"] for m in workers_data) / len(workers_data)
    print(f"\n[2] 汇总: total_steps={total_steps}, avg_reward={avg_reward:.2f}")

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
