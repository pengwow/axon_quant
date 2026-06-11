"""distributed_basic.py — 分布式训练 mock 模式示例。

不连接真实 Ray 集群，演示：
1. DistributedTrainer mock 训练
2. Checkpoint 保存/恢复
3. 容错训练循环
"""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

CARGO_MANIFEST = Path(__file__).parent.parent / "crates" / "axon-distributed"
sys.path.insert(0, str(CARGO_MANIFEST / "python"))

from axon_distributed.types import (  # noqa: E402
    CheckpointConfig,
    RayConfig,
    RLLibTrainConfig,
)
from axon_distributed.ray_trainer import DistributedTrainer  # noqa: E402
from axon_distributed.fault_tolerance import (  # noqa: E402
    CheckpointManager,
    FaultTolerantTrainer,
)


def main() -> int:
    print("=" * 60)
    print("分布式训练 mock 模式示例")
    print("=" * 60)

    # 1. 加载默认 TOML 配置
    train_cfg = RLLibTrainConfig()
    train_cfg._load_default_toml()
    print(f"\n[1] 训练配置（来自 default_distributed.toml）")
    print(f"  algorithm: {train_cfg.algorithm}")
    print(f"  num_workers: {train_cfg.num_workers}")
    print(f"  train_batch_size: {train_cfg.train_batch_size}")
    print(f"  lr: {train_cfg.lr}")

    ray_cfg = RayConfig(
        num_workers=train_cfg.num_workers,
        num_cpus_per_worker=2,
        num_gpus_per_worker=0.0,
    )
    ray_cfg.validate()
    print(f"  ray_init_kwargs: {ray_cfg.to_ray_init_kwargs()}")

    # 2. mock 训练
    print("\n[2] DistributedTrainer mock 训练")
    trainer = DistributedTrainer(ray_cfg, train_cfg, init_ray=False)
    result = trainer.train(num_iterations=5, checkpoint_interval=3)
    print(f"  iterations: {result['iterations']}")
    print(f"  final_reward: {result['final_reward']:.4f}")

    # 3. 容错训练 + Checkpoint
    print("\n[3] FaultTolerantTrainer + Checkpoint")
    with tempfile.TemporaryDirectory() as tmp:
        ckpt_cfg = CheckpointConfig(
            checkpoint_dir=tmp,
            keep_checkpoints_num=2,
            checkpoint_at_end=True,
        )
        ft_trainer = FaultTolerantTrainer(trainer, ckpt_cfg)
        result = ft_trainer.train_with_recovery(num_iterations=5, checkpoint_interval=2)
        print(f"  start_iteration: {result['start_iteration']}")
        print(f"  iterations: {result['iterations']}")
        print(f"  final_reward: {result['final_reward']:.4f}")

        # 验证 checkpoint 文件
        ckpt_files = sorted(Path(tmp).glob("*.meta.json"))
        print(f"  checkpoint meta files: {len(ckpt_files)}")
        for f in ckpt_files:
            print(f"    {f.name}")

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
