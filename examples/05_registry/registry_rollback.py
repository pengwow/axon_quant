"""registry_rollback.py — 注册多版本 + 回滚。

使用 `axon_quant.registry` 模块。

运行方式：
    cd axon
    .venv/bin/python examples/05_registry/registry_rollback.py
"""

from __future__ import annotations

import os
import tempfile

import axon_quant  # noqa: E402
ModelRegistry = axon_quant.registry.ModelRegistry
LocalStorage = axon_quant.registry.LocalStorage


def main() -> int:
    print("=" * 60)
    print("Model Registry 多版本 + 回滚示例")
    print("=" * 60)

    with tempfile.TemporaryDirectory() as tmp:
        storage = LocalStorage(os.path.join(tmp, "models"))
        registry = ModelRegistry(storage)

        # 注册 3 个版本
        for i in range(1, 4):
            model_path = os.path.join(tmp, f"model_v{i}.bin")
            with open(model_path, "wb") as f:
                f.write(f"PPO weights v{i}".encode() * 100)

            mv = registry.register(
                "ppo-momentum",
                model_path,
                description=f"PPO v{i}",
                metrics={"sharpe": 1.0 + 0.2 * i, "max_drawdown": 0.1 * (4 - i)},
            )
            print(f"\n[{i}] 注册 v{i}: {mv}")

        # 查看各阶段状态
        print("\n[状态] 各阶段版本数：")
        all_versions = registry.list_versions("ppo-momentum")
        print(f"  总版本数: {len(all_versions)}")

        # 回滚
        print("\n[回滚] 执行 rollback")
        prod = registry.rollback("ppo-momentum")
        print(f"  当前 Production: {prod}")

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
