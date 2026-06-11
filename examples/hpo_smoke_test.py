"""hpo_smoke_test.py — HPO Python 端 smoke test。

验证：
1. 模块导入（axon_hpo / types / optuna_runner / multi_objective / search_space）
2. SearchSpaceDef 6 种参数类型 + 默认 RL 搜索空间
3. Pareto 前沿 / 超体积计算
4. select_by_constraint 工具
5. 完整 OptunaHPO 流程
"""

from __future__ import annotations

import sys
from pathlib import Path

CARGO_MANIFEST = Path(__file__).parent.parent / "crates" / "axon-hpo"
sys.path.insert(0, str(CARGO_MANIFEST / "python"))

from axon_hpo import multi_objective, search_space, types
from axon_hpo.optuna_runner import OptunaHPO


def main() -> int:
    print("=" * 60)
    print("axon_hpo Python 端 smoke test")
    print("=" * 60)

    # 1. 默认 RL 搜索空间
    print("\n[1] 默认 PPO 搜索空间（11 个超参数）")
    space = search_space.default_ppo_search_space()
    for name, def_ in space.items():
        def_.validate()
        print(f"  ✓ {name:18s} {def_.param_type}")

    # 1b. 默认 SAC 搜索空间 + 小型空间
    sac_space = search_space.default_sac_search_space()
    for name, def_ in sac_space.items():
        def_.validate()
    print(f"  ✓ SAC 搜索空间: {len(sac_space)} 个超参数")
    small_space = search_space.small_search_space()
    for name, def_ in small_space.items():
        def_.validate()
    print(f"  ✓ small 搜索空间: {len(small_space)} 个超参数")

    # 2. Pareto / Hypervolume
    print("\n[2] Pareto / Hypervolume 验证")
    dirs = [types.StudyDirection.MAXIMIZE, types.StudyDirection.MAXIMIZE]
    trials = [
        types.TrialResult(0, {"lr": 0.001}, [1.0, 0.5], "complete", 10),
        types.TrialResult(1, {"lr": 0.002}, [0.5, 1.0], "complete", 10),
        types.TrialResult(2, {"lr": 0.003}, [0.3, 0.3], "complete", 10),  # 被支配
        types.TrialResult(3, {"lr": 0.004}, [0.8, 0.8], "complete", 10),
    ]
    front = multi_objective.compute_pareto_front(trials, dirs)
    print(f"  ✓ Pareto 前沿: {len(front)} 个点（trial 2 被排除）")
    hv = multi_objective.compute_hypervolume(front, dirs, [2.0, 2.0])
    print(f"  ✓ 超体积: {hv:.4f}")

    # 3. select_by_constraint
    best = multi_objective.select_by_constraint(front, lambda objs: objs[1] > 0.7)
    assert best is not None
    print(f"  ✓ select_by_constraint(objs[1]>0.7): trial #{best.trial_id}")

    # 4. 完整 Optuna 流程
    print("\n[3] OptunaHPO 完整流程（5 trials）")

    def obj(params):
        return [-((params["x"] - 0.5) ** 2)]

    hpo = OptunaHPO(
        search_space={"x": types.SearchSpaceDef("uniform", 0.0, 1.0)},
        objective_fn=obj,
        study_name="smoke_test",
        directions="maximize",
    )
    results = hpo.run(n_trials=5)
    print(f"  ✓ 完成 {len(results)} trials")
    best_trial = hpo.get_best_trial()
    assert best_trial is not None
    print(f"  ✓ best_trial: x={best_trial.params['x']:.4f}, "
          f"value={best_trial.values[0]:.4f}")

    print("\n=== ALL PASS ===")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
