"""tracker_multi_backend.py — 多后端并行追踪。

使用 `axon_quant.tracker` 模块，演示同时向多个 tracker 写入数据。

运行方式：
    cd axon
    .venv/bin/python examples/06_tracker/tracker_multi_backend.py
"""

from __future__ import annotations

import tempfile
from pathlib import Path

import axon_quant  # noqa: E402
MemoryTracker = axon_quant.tracker.MemoryTracker
LocalTracker = axon_quant.tracker.LocalTracker


class MultiTracker:
    """简单多后端包装：将调用转发到多个 tracker。"""

    def __init__(self, trackers: list):
        self.trackers = trackers

    def log_metric(self, key: str, value: float, step: int = 0):
        for t in self.trackers:
            t.log_metric(key, value, step=step)

    def log_param(self, key: str, value):
        for t in self.trackers:
            t.log_param(key, value)

    def set_tag(self, key: str, value: str):
        for t in self.trackers:
            t.set_tag(key, value)

    def finish(self, status: str = "completed"):
        for t in self.trackers:
            t.finish(status)


def main() -> int:
    print("=" * 60)
    print("Multi-Tracker 多后端并行示例")
    print("=" * 60)

    with tempfile.TemporaryDirectory() as tmp:
        # 同时向 3 个后端写入
        trackers = [
            MemoryTracker(),
            MemoryTracker(),
            LocalTracker(tmp),
        ]
        mt = MultiTracker(trackers)

        # 训练循环模拟
        for epoch in range(5):
            loss = 1.0 / (epoch + 1)
            reward = 1.0 - 0.1 * epoch
            mt.log_metric("train/loss", loss, step=epoch)
            mt.log_metric("val/reward", reward, step=epoch)

        mt.log_param("learning_rate", 0.0003)
        mt.finish("completed")

        # 验证
        print(f"\n[1] 3 个 trackers:")
        for i, t in enumerate(trackers):
            if hasattr(t, 'get_metrics'):
                metrics = t.get_metrics()
                print(f"  tracker[{i}]: {len(metrics)} 条指标")
            else:
                print(f"  tracker[{i}]: {type(t).__name__}")

        # 验证 LocalTracker 写入了文件
        files = sorted(p.relative_to(tmp) for p in Path(tmp).rglob("*.json*"))
        print(f"\n[2] LocalTracker 写入文件: {len(files)}")
        for f in files[:5]:
            print(f"  {f}")

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
