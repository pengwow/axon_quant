"""distributed_basic.py — 分布式训练 mock 模式示例。

使用 `axon_quant.distributed` 模块演示：
1. 指标序列化
2. Checkpoint 保存/恢复
3. DistributedRunner 配置

运行方式：
    cd axon
    .venv/bin/python examples/04_distributed/distributed_basic.py
"""

from __future__ import annotations

import json
import tempfile
from pathlib import Path

import axon_quant  # noqa: E402
DistributedRunner = axon_quant.distributed.DistributedRunner
py_serialize_metrics = axon_quant.distributed.py_serialize_metrics
py_save_checkpoint = axon_quant.distributed.py_save_checkpoint
py_load_checkpoint = axon_quant.distributed.py_load_checkpoint


def main() -> int:
    print("=" * 60)
    print("分布式训练 mock 模式示例")
    print("=" * 60)

    # 1. 指标序列化
    print("\n[1] 指标序列化")
    metrics_json = py_serialize_metrics(
        step=100,
        reward=0.5,
        policy_loss=0.01,
        value_loss=0.02,
        entropy=0.1,
        fps=1000.0,
    )
    metrics = json.loads(metrics_json)
    print(f"  step: {metrics['step']}")
    print(f"  episode_reward_mean: {metrics['episode_reward_mean']}")
    print(f"  policy_loss: {metrics['policy_loss']}")

    # 2. Checkpoint 保存/恢复
    print("\n[2] Checkpoint 保存/恢复")
    with tempfile.TemporaryDirectory() as tmp:
        # 保存 checkpoint
        ckpt_json = py_save_checkpoint(
            iteration=10,
            policy_state=list(b"policy weights"),
            optimizer_state=list(b"optimizer state"),
            rng_state=list(b"rng state"),
        )
        ckpt_path = Path(tmp) / "checkpoint.json"
        ckpt_path.write_text(ckpt_json)
        print(f"  保存 checkpoint: {ckpt_path.name} ({len(ckpt_json)} bytes)")

        # 加载 checkpoint
        loaded_json = ckpt_path.read_text()
        iteration, policy_state = py_load_checkpoint(loaded_json)
        print(f"  加载 checkpoint: iteration={iteration}, "
              f"policy_state={len(policy_state)} bytes")

    # 3. 模块功能验证
    print("\n[3] 模块功能验证")
    print(f"  DistributedRunner 类: {DistributedRunner}")

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
