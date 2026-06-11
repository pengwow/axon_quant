"""distributed_actor_pool.py — ActorPool 用法示例（mock 模式）。"""

from __future__ import annotations

import sys
from pathlib import Path

CARGO_MANIFEST = Path(__file__).parent.parent / "crates" / "axon-distributed"
sys.path.insert(0, str(CARGO_MANIFEST / "python"))

from axon_distributed.actor import ActorPool, RAY_AVAILABLE  # noqa: E402


def main() -> int:
    print("=" * 60)
    print("ActorPool 示例（mock 模式）")
    print("=" * 60)
    print(f"RAY_AVAILABLE: {RAY_AVAILABLE}")

    # 1. 创建 ActorPool
    pool = ActorPool(
        num_workers=2,
        env_class="AxonTradingEnv",
        env_config={"data_path": "mock.parquet"},
        num_envs_per_worker=2,
        observation_space_shape=(10, 60),
        action_space_shape=(1,),
    )
    print(f"\n[1] 创建 ActorPool: {len(pool.workers)} 个 workers")

    # 2. reset_all
    obs_list = pool.reset_all()
    print(f"[2] reset_all 返回: {len(obs_list)} 个 worker 的初始观测")
    for obs in obs_list:
        print(f"  worker {obs['worker_id']}: {len(obs['observations'])} envs")

    # 3. step_all
    actions_list = [[0] * 2] * len(pool.workers)  # 每个 worker 2 个 env 的 action
    results = pool.step_all(actions_list)
    print(f"[3] step_all 返回: {len(results)} 个 worker 的 step 结果")
    for r in results:
        print(f"  worker {r['worker_id']}: rewards={r['rewards']}")

    # 4. get_all_metrics
    metrics = pool.get_all_metrics()
    print(f"[4] get_all_metrics 返回: {len(metrics)} 个 WorkerMetrics")
    for m in metrics:
        print(
            f"  worker {m.worker_id}: "
            f"avg_reward={m.avg_reward:.4f}, total_steps={m.total_steps}"
        )

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
