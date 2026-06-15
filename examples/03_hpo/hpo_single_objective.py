"""hpo_single_objective.py — 单目标 HPO 示例。

使用 `axon_quant.hpo.py_compute_pareto_front` 和
`axon_quant.hpo.py_compute_hypervolume` 验证单目标优化的 Pareto 计算。

运行方式：
    cd axon
    .venv/bin/python examples/03_hpo/hpo_single_objective.py
"""

from __future__ import annotations

import axon_quant  # noqa: E402
hpo = axon_quant.hpo


def main() -> int:
    print("=" * 60)
    print("单目标 HPO 示例")
    print("=" * 60)

    # 1. 生成 trials：目标函数 -(x-0.5)^2 - (y-0.3)^2
    print("\n[1] 生成 20 个 trials")
    trials = []
    for i in range(20):
        x = i / 20.0
        y = 1.0 - i / 20.0
        score = -((x - 0.5) ** 2) - ((y - 0.3) ** 2)
        trials.append({
            "trial_id": i,
            "values": [score],
        })

    best = max(trials, key=lambda t: t["values"][0])
    print(f"  最佳 trial: #{best['trial_id']}, value={best['values'][0]:.4f}")

    # 2. 单目标 Pareto
    print("\n[2] 单目标 Pareto 前沿")
    front = hpo.py_compute_pareto_front(trials, ["maximize"])
    print(f"  前沿点数: {len(front)}（应为 1）")
    assert len(front) == 1
    print(f"  最优点: trial #{front[0]['trial_id']}, "
          f"value={front[0]['objectives'][0]:.4f}")

    # 3. 多目标 Pareto
    print("\n[3] 多目标 Pareto 前沿")
    multi_trials = [
        {"trial_id": i, "values": [float(i), float(20 - i)]}
        for i in range(21)
    ]
    front = hpo.py_compute_pareto_front(multi_trials, ["maximize", "maximize"])
    print(f"  前沿点数: {len(front)}（应为 21，所有点互不支配）")
    assert len(front) == 21

    # 4. 超体积
    print("\n[4] 超体积计算")
    hv = hpo.py_compute_hypervolume(front, ["maximize", "maximize"], [21.0, 21.0])
    print(f"  超体积: {hv:.4f}")
    assert hv > 0

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
