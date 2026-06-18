"""registry_register_promote.py — 注册 + 提升到 Production。

使用 `axon_quant.registry` 模块。

运行方式：
    cd axon
    .venv/bin/python examples/05_registry/registry_register_promote.py
"""

from __future__ import annotations

import os
import tempfile

import axon_quant  # noqa: E402
ModelRegistry = axon_quant.registry.ModelRegistry
LocalStorage = axon_quant.registry.LocalStorage


def main() -> int:
    print("=" * 60)
    print("Model Registry 注册 + 提升到 Production")
    print("=" * 60)

    with tempfile.TemporaryDirectory() as tmp:
        # 准备源文件
        model_path = os.path.join(tmp, "model_v1.bin")
        with open(model_path, "wb") as f:
            f.write(b"PPO policy weights v1 (1024 params)")

        # 创建存储 + 注册表
        storage = LocalStorage(os.path.join(tmp, "models"))
        registry = ModelRegistry(storage)

        # 注册 v1
        mv1 = registry.register(
            "ppo-momentum",
            model_path,
            description="PPO momentum strategy v1",
            metrics={"sharpe": 1.5, "max_drawdown": 0.12},
        )
        print(f"\n[1] 注册 v1: {mv1}")

        # 获取 Production 版本
        prod = registry.get_production("ppo-momentum")
        print(f"\n[2] 当前 Production: {prod}")

        # 列出所有版本
        all_versions = registry.list_versions("ppo-momentum")
        print(f"\n[3] 所有版本: {len(all_versions)}")
        for v in all_versions:
            print(f"   - {v}")

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
