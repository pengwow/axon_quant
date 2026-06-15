"""hpo_smoke_test.py — HPO Python 端 smoke test。

使用 `axon_quant.hpo` 模块验证：
1. 模块导入
2. Pareto 前沿 / 超体积计算
3. 完整 HPO 流程

运行方式：
    cd axon
    .venv/bin/python examples/03_hpo/hpo_smoke_test.py
"""

from __future__ import annotations

import axon_quant  # noqa: E402
hpo = axon_quant.hpo


def main() -> int:
    print("=" * 60)
    print("axon_quant.hpo smoke test")
    print("=" * 60)

    # 1. 模块功能验证
    print("\n[1] 模块功能验证")
    print(f"  ✓ HPORunner: {hpo.HPORunner}")
    print(f"  ✓ py_compute_pareto_front: {hpo.py_compute_pareto_front}")
    print(f"  ✓ py_compute_hypervolume: {hpo.py_compute_hypervolume}")
    print(f"  ✓ py_validate_search_space: {hpo.py_validate_search_space}")

    # 2. Pareto / Hypervolume
    print("\n[2] Pareto / Hypervolume 验证")
    trials = [
        {"trial_id": 0, "values": [1.0, 0.5]},
        {"trial_id": 1, "values": [0.5, 1.0]},
        {"trial_id": 2, "values": [0.3, 0.3]},  # 被支配
        {"trial_id": 3, "values": [0.8, 0.8]},
    ]
    front = hpo.py_compute_pareto_front(trials, ["maximize", "maximize"])
    print(f"  ✓ Pareto 前沿: {len(front)} 个点（trial 2 被排除）")
    assert len(front) == 3

    hv = hpo.py_compute_hypervolume(front, ["maximize", "maximize"], [2.0, 2.0])
    print(f"  ✓ 超体积: {hv:.4f}")
    assert hv > 0

    # 3. 搜索空间校验
    print("\n[3] 搜索空间校验")
    valid = hpo.py_validate_search_space(
        '{"type": "uniform", "low": 0.0, "high": 1.0}'
    )
    print(f"  ✓ 校验结果: {valid}")

    # 4. 单目标 Pareto（只有一个最优点）
    print("\n[4] 单目标 Pareto")
    single_trials = [
        {"trial_id": i, "values": [float(i)]} for i in range(5)
    ]
    front = hpo.py_compute_pareto_front(single_trials, ["maximize"])
    print(f"  ✓ 单目标前沿: {len(front)} 个点（应为 1）")
    assert len(front) == 1

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
