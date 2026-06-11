"""hpo_single_objective.py — 单目标 HPO 示例。

目标函数是 `-(x - 0.5)^2 - (y - 0.3)^2`（在 x=0.5, y=0.3 处取最大值 0）。
HPO 应能找到接近 (0.5, 0.3) 的参数。

运行：
    $PY examples/hpo_single_objective.py
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

# 让 Python 找到 axon_hpo 包
CARGO_MANIFEST = Path(__file__).parent.parent / "crates" / "axon-hpo"
sys.path.insert(0, str(CARGO_MANIFEST / "python"))

import axon_hpo  # noqa: E402  （仅检查可导入）
import axon_hpo.types as hpo_types  # noqa: E402
import axon_hpo.search_space as hpo_space  # noqa: E402
import axon_hpo.optuna_runner as hpo_runner  # noqa: E402
import axon_hpo.multi_objective as hpo_mo  # noqa: E402


def main() -> int:
    print("=" * 60)
    print("单目标 HPO 示例")
    print("=" * 60)
    print(f"axon_hpo 版本: {axon_hpo.__version__}")

    # 定义搜索空间
    space = {
        "x": hpo_types.SearchSpaceDef(param_type="uniform", low=0.0, high=1.0),
        "y": hpo_types.SearchSpaceDef(param_type="uniform", low=0.0, high=1.0),
    }

    # 目标函数：二维抛物面（最大值在 (0.5, 0.3)）
    def objective(params: dict[str, float]) -> list[float]:
        x = params["x"]
        y = params["y"]
        score = -((x - 0.5) ** 2) - ((y - 0.3) ** 2)
        return [score]

    # 构造 HPO runner
    hpo = hpo_runner.OptunaHPO(
        search_space=space,
        objective_fn=objective,
        study_name="parabola_single",
        directions="maximize",
    )

    # 运行 30 个 trial
    t0 = time.perf_counter()
    results = hpo.run(n_trials=30)
    elapsed = time.perf_counter() - t0

    print(f"\n✓ 完成 {len(results)} 个 trial，耗时 {elapsed:.2f}s")
    complete = [r for r in results if r.state == "complete"]
    print(f"  Complete: {len(complete)}, "
          f"Pruned: {sum(1 for r in results if r.state == 'pruned')}, "
          f"Fail: {sum(1 for r in results if r.state == 'fail')}")

    # 最佳 trial
    best = hpo.get_best_trial()
    if best is not None:
        print(f"\n最佳 trial #{best.trial_id}:")
        print(f"  params: {best.params}")
        print(f"  value:  {best.values[0]:.6f}")
        # 期望 x ≈ 0.5, y ≈ 0.3, value ≈ 0.0
        ok = abs(best.params["x"] - 0.5) < 0.15 and abs(best.params["y"] - 0.3) < 0.15
        print(f"  接近全局最优 (0.5, 0.3): {'PASS' if ok else 'FAIL'}")

    # 验证 Pareto / hypervolume 在单目标场景下也可用
    directions = [hpo_types.StudyDirection.MAXIMIZE]
    front = hpo_mo.compute_pareto_front(complete, directions)
    print(f"\nPareto 前沿: {len(front)} 个点（单目标场景下 = 最佳 trial）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
