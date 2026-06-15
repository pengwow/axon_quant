"""tracker_basic.py — Memory + Local Tracker 基本用法。

使用 `axon_quant.tracker` 模块。

运行方式：
    cd axon
    .venv/bin/python examples/06_tracker/tracker_basic.py
"""

from __future__ import annotations

import tempfile
from pathlib import Path

import axon_quant  # noqa: E402
MemoryTracker = axon_quant.tracker.MemoryTracker
LocalTracker = axon_quant.tracker.LocalTracker


def main() -> int:
    print("=" * 60)
    print("Tracker 基本用法示例")
    print("=" * 60)

    # 1. Memory Tracker
    print("\n[1] MemoryTracker")
    mt = MemoryTracker()
    mt.log_param("learning_rate", 0.001)
    mt.log_param("batch_size", 256)
    mt.log_param("algorithm", "PPO")
    mt.log_metric("train/loss", 0.5, step=0)
    mt.log_metric("train/loss", 0.4, step=1)
    mt.log_metric("val/reward", 1.2, step=0)
    mt.set_tag("strategy", "momentum")
    mt.set_tag("market_regime", "high_volatility")
    metrics = mt.get_metrics()
    print(f"  params logged: lr, batch_size, algorithm")
    print(f"  metrics logged: {len(metrics)} 条")
    print(f"  status: running")

    # 2. Local Tracker
    print("\n[2] LocalTracker")
    with tempfile.TemporaryDirectory() as tmp:
        lt = LocalTracker(tmp)
        lt.log_param("learning_rate", 0.0003)
        lt.log_metric("train/loss", 0.5, step=0)
        lt.log_metric("train/loss", 0.4, step=1)
        lt.log_metric("val/reward", 1.2, step=0)
        lt.flush()
        files = sorted(p.relative_to(tmp) for p in Path(tmp).rglob("*") if p.is_file())
        print(f"  files written: {[str(f) for f in files]}")

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
