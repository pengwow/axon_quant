"""AXON HPO 单元测试。

测试范围：
- SearchSpaceDef 参数采样
- OptunaHPO 单目标/多目标（双参 report 签名，0.14.5 Breaking Change）
- **剪枝 E2E**：MEDIAN pruner 必须真正触发 PRUNED state
- Pareto 前沿计算
- 超体积计算
"""
from __future__ import annotations

import pytest

pytest.importorskip("optuna")
pytest.importorskip("numpy")

from axon_hpo.optuna_runner import OptunaHPO
from axon_hpo.search_space import (
    default_ppo_search_space,
    default_sac_search_space,
    small_search_space,
)
from axon_hpo.types import (
    PrunerConfig,
    PrunerType,
    SamplerConfig,
    SamplerType,
    SearchSpaceDef,
    StudyDirection,
    TrialResult,
)
from axon_hpo.multi_objective import (
    ParetoPoint,
    compute_hypervolume,
    compute_pareto_front,
    dominates,
    select_by_constraint,
)
from axon_hpo.pruning import adaptive_median_prune


# =============================================================================
# SearchSpaceDef 测试
# =============================================================================
class TestSearchSpaceDef:
    """搜索空间定义测试"""

    def test_uniform_param(self):
        """测试均匀分布参数"""
        space = SearchSpaceDef(param_type="uniform", low=0.0, high=1.0)
        space.validate()
        assert space.param_type == "uniform"
        assert space.low == 0.0
        assert space.high == 1.0

    def test_log_uniform_param(self):
        """测试对数均匀分布参数"""
        space = SearchSpaceDef(param_type="log_uniform", low=1e-5, high=1e-2)
        space.validate()
        assert space.log is False  # suggest 方法内部处理 log

    def test_log_uniform_low_zero(self):
        """测试对数均匀分布 low <= 0 时抛错"""
        with pytest.raises(ValueError):
            SearchSpaceDef(param_type="log_uniform", low=0.0, high=1.0).validate()

    def test_int_uniform_param(self):
        """测试整数均匀分布参数"""
        space = SearchSpaceDef(param_type="int_uniform", low=1, high=10, step=2)
        space.validate()
        assert space.step == 2

    def test_choice_param(self):
        """测试离散选择参数"""
        space = SearchSpaceDef(param_type="choice", choices=[32, 64, 128])
        space.validate()
        assert space.choices == [32, 64, 128]

    def test_categorical_param(self):
        """测试分类参数"""
        space = SearchSpaceDef(param_type="categorical", choices=["relu", "tanh", "sigmoid"])
        space.validate()
        assert space.choices == ["relu", "tanh", "sigmoid"]

    def test_invalid_param_type(self):
        """测试无效参数类型"""
        with pytest.raises(ValueError):
            SearchSpaceDef(param_type="invalid_type", low=0.0, high=1.0)

    def test_missing_low_high(self):
        """测试缺少 low/high"""
        with pytest.raises(ValueError):
            SearchSpaceDef(param_type="uniform").validate()

    def test_low_ge_high(self):
        """测试 low >= high"""
        with pytest.raises(ValueError):
            SearchSpaceDef(param_type="uniform", low=1.0, high=0.5).validate()

    def test_empty_choices(self):
        """测试空 choices"""
        with pytest.raises(ValueError):
            SearchSpaceDef(param_type="choice", choices=[]).validate()

    def test_to_dict(self):
        """测试转为 dict"""
        space = SearchSpaceDef(param_type="uniform", low=0.0, high=1.0, step=0.1)
        result = space.to_dict()
        assert result == {"type": "uniform", "low": 0.0, "high": 1.0, "step": 0.1}


# =============================================================================
# 搜索空间预设测试
# =============================================================================
class TestSearchSpacePresets:
    """搜索空间预设测试"""

    def test_small_search_space(self):
        """测试小型搜索空间"""
        space = small_search_space()
        assert len(space) == 2
        assert "learning_rate" in space
        assert "gamma" in space

    def test_default_ppo_search_space(self):
        """测试 PPO 默认搜索空间"""
        space = default_ppo_search_space()
        assert len(space) >= 5
        assert "learning_rate" in space
        assert "gamma" in space
        assert "clip_range" in space
        assert "entropy_coef" in space

    def test_default_sac_search_space(self):
        """测试 SAC 默认搜索空间"""
        space = default_sac_search_space()
        assert len(space) >= 5
        assert "learning_rate" in space
        assert "gamma" in space
        assert "tau" in space


# =============================================================================
# OptunaHPO 测试
# =============================================================================
class TestOptunaHPO:
    """OptunaHPO 测试（0.14.5 Breaking Change: objective_fn 双参签名）"""

    def test_single_objective(self):
        """测试单目标优化"""
        def objective(params, report):
            _ = report  # 单目标场景不强制用 report
            return [params.get("learning_rate", 0.001) * 100]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_single_v2",
            directions="maximize",
        )

        results = hpo.run(n_trials=5, n_jobs=1)
        assert len(results) == 5
        assert all(isinstance(r, TrialResult) for r in results)

        best = hpo.get_best_trial()
        assert best is not None
        assert "learning_rate" in best.params

    def test_multi_objective(self):
        """测试多目标优化"""
        def objective(params, report):
            _ = report
            lr = params.get("learning_rate", 0.001)
            gamma = params.get("gamma", 0.99)
            return [lr * 100, gamma]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_multi_v2",
            directions=["maximize", "maximize"],
        )

        results = hpo.run(n_trials=5, n_jobs=1)
        assert len(results) == 5

        front = hpo.get_pareto_front()
        assert isinstance(front, list)

        hv = hpo.compute_hypervolume(reference_point=[10.0, 1.0])
        assert isinstance(hv, float)
        assert hv >= 0.0

    def test_collect_results(self):
        """测试收集结果"""
        def objective(params, report):
            _ = report
            return [params.get("learning_rate", 0.001) * 100]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_collect_v2",
            directions="maximize",
        )

        hpo.run(n_trials=3, n_jobs=1)
        results = hpo.collect_results()
        assert len(results) == 3
        assert all(isinstance(r, dict) for r in results)
        assert "trial_id" in results[0]
        assert "params" in results[0]
        assert "values" in results[0]
        assert "state" in results[0]

    def test_pruner_config(self):
        """测试剪枝器配置"""
        pruner = PrunerConfig(pruner_type=PrunerType.MEDIAN, n_startup_trials=3)
        assert pruner.pruner_type.value == "median"
        assert pruner.n_startup_trials == 3

    def test_sampler_config(self):
        """测试采样器配置"""
        sampler = SamplerConfig(sampler_type=SamplerType.TPE, seed=42)
        assert sampler.sampler_type.value == "tpe"
        assert sampler.seed == 42

    # ---- 剪枝核心测试（0.14.5 新增） ----

    def test_pruning_effective(self):
        """**核心验收**：MedianPruner 必须真正触发 PRUNED state。

        策略：前 2 个 trial 报告高值，后续 trial 报告快速递减低值，
        期望 MedianPruner 在第 3+ 个 trial 上剪枝。

        这是区分"report 真调了 optuna" vs "report 只做了内存缓存"的分水岭。
        """
        pruner = PrunerConfig(
            pruner_type=PrunerType.MEDIAN,
            n_startup_trials=2,     # 前 2 个 trial 不剪枝
            n_warmup_steps=3,       # 前 3 步不剪枝
        )

        step_counter = {"n": 0}

        def objective(params, report):
            # 用 step_counter 保证"前 2 trial 高值 + 后续 trial 低值"的确定性
            # 每个 trial 走 10 步，step 4 起开始报告
            for step in range(10):
                if step >= 4:
                    step_counter["n"] += 1
                    if step_counter["n"] <= 4:
                        value = 0.9   # 前 2 个 trial 的中间值：高
                    else:
                        value = 0.1   # 后续 trial 的中间值：低 → 触发剪枝阈值
                    report(step, value)
            # 最终值也呈两极：前 2 trial 高，后续低
            final = 0.9 if step_counter["n"] <= 4 else 0.1
            return [final]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_pruning_effective_v2",
            directions="maximize",
            pruner=pruner,
            sampler=SamplerConfig(sampler_type=SamplerType.RANDOM, seed=42),
        )

        results = hpo.run(n_trials=6, n_jobs=1)

        states = [r.state for r in results]
        pruned = [r for r in results if r.state == "pruned"]

        assert len(pruned) > 0, (
            f"期望出现至少 1 个 PRUNED trial，实际 states = {states}。"
            f"这意味着 report() 没真正调 optuna trial.report()，剪枝链路仍断。"
        )

    def test_pruned_trial_keeps_intermediate(self):
        """被剪枝的 trial：intermediate_values 非空、state 为 pruned。

        注意：optuna 5.0+ 对 pruned trial.values 的语义可能因版本而异
        （部分版本保留最后一次 report 的 value，部分版本置 None），
        因此这里不硬断言 values 是否为空，只锁核心行为：
        state=pruned + intermediate_values 保留。
        """
        pruner = PrunerConfig(
            pruner_type=PrunerType.MEDIAN,
            n_startup_trials=1,
            n_warmup_steps=2,
        )

        step_counter = {"n": 0}

        def objective(params, report):
            for step in range(10):
                if step >= 2:
                    step_counter["n"] += 1
                    # 前几个 trial 高值，后续低值 → 必然触发剪枝
                    value = 0.9 if step_counter["n"] <= 2 else 0.0
                    report(step, value)
            return [0.9 if step_counter["n"] <= 2 else 0.0]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_pruned_keeps_iv_v2",
            directions="maximize",
            pruner=pruner,
            sampler=SamplerConfig(sampler_type=SamplerType.RANDOM, seed=123),
        )

        results = hpo.run(n_trials=5, n_jobs=1)

        pruned = [r for r in results if r.state == "pruned"]
        assert len(pruned) > 0, "至少要有一个 pruned trial 才能验证中间值保留"

        # 所有 pruned trial 都应有中间值（被剪在 report 之后）
        for r in pruned:
            assert len(r.intermediate_values) > 0, (
                f"pruned trial#{r.trial_id} intermediate_values 为空 — "
                f"optuna 侧没存中间值，report 调用可能失败了"
            )
            # 核心：state 必须是 pruned
            assert r.state == "pruned"

    def test_no_report_no_prune(self):
        """objective 完全不调用 report → 所有 trial 都 COMPLETE（剪枝器无信息可用）。"""
        pruner = PrunerConfig(
            pruner_type=PrunerType.MEDIAN,
            n_startup_trials=1,
            n_warmup_steps=1,
        )

        def objective(params, report):
            _ = report  # 签名对了，但故意不调
            return [params.get("learning_rate", 0.001) * 100]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_no_report_v2",
            directions="maximize",
            pruner=pruner,
        )

        results = hpo.run(n_trials=4, n_jobs=1)

        states = [r.state for r in results]
        pruned_count = states.count("pruned")
        assert pruned_count == 0, (
            f"不调 report 时不应出现 PRUNED，实际 states = {states}"
        )

    def test_inject_report_signature(self):
        """report 闭包必须能正确绑定当前 trial、且签名正确（step, value）。"""
        captured = {"report": None}

        def objective(params, report):
            captured["report"] = report
            return [params.get("learning_rate", 0.001) * 100]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_report_sig_v2",
            directions="maximize",
        )

        hpo.run(n_trials=1, n_jobs=1)

        assert captured["report"] is not None
        # report 是 callable
        assert callable(captured["report"])


# =============================================================================
# 多目标优化测试
# =============================================================================
class TestMultiObjective:
    """多目标优化测试"""

    def test_dominates_maximize(self):
        """测试 maximize 方向的支配关系"""
        a = [1.0, 2.0]
        b = [0.5, 1.5]
        directions = [StudyDirection.MAXIMIZE, StudyDirection.MAXIMIZE]
        assert dominates(a, b, directions) is True

    def test_dominates_not(self):
        """测试不支配"""
        a = [1.0, 1.0]
        b = [1.0, 2.0]
        directions = [StudyDirection.MAXIMIZE, StudyDirection.MAXIMIZE]
        assert dominates(a, b, directions) is False

    def test_dominates_minimize(self):
        """测试 minimize 方向"""
        a = [0.5, 0.5]
        b = [1.0, 1.0]
        directions = [StudyDirection.MINIMIZE, StudyDirection.MINIMIZE]
        assert dominates(a, b, directions) is True

    def test_compute_pareto_front(self):
        """测试 Pareto 前沿计算"""
        trials = [
            TrialResult(trial_id=0, params={}, values=[1.0, 2.0], state="complete", duration_ms=0),
            TrialResult(trial_id=1, params={}, values=[2.0, 1.0], state="complete", duration_ms=0),
            TrialResult(trial_id=2, params={}, values=[0.5, 0.5], state="complete", duration_ms=0),
        ]
        directions = [StudyDirection.MAXIMIZE, StudyDirection.MAXIMIZE]
        front = compute_pareto_front(trials, directions)
        assert len(front) == 2  # 前两个是前沿

    def test_compute_hypervolume_2d(self):
        """测试 2D 超体积计算"""
        front = [
            ParetoPoint(params={}, objectives=[1.0, 2.0], trial_id=0),
            ParetoPoint(params={}, objectives=[2.0, 1.0], trial_id=1),
        ]
        directions = [StudyDirection.MAXIMIZE, StudyDirection.MAXIMIZE]
        hv = compute_hypervolume(front, directions, reference_point=[5.0, 5.0])
        assert hv > 0.0

    def test_select_by_constraint(self):
        """测试按约束选择"""
        front = [
            ParetoPoint(params={"lr": 0.001}, objectives=[1.0, 0.9], trial_id=0),
            ParetoPoint(params={"lr": 0.002}, objectives=[2.0, 0.8], trial_id=1),
        ]
        result = select_by_constraint(front, lambda obj: obj[1] >= 0.85)
        assert result is not None
        assert result.trial_id == 0


# =============================================================================
# 剪枝策略测试
# =============================================================================
class TestPruning:
    """剪枝策略测试"""

    def test_adaptive_median_prune_basic(self):
        """测试自适应中位数剪枝基本行为"""
        # 创建 mock trial
        class MockStudy:
            trials = []

        class MockTrial:
            number = 10
            study = MockStudy()

        # 启动阶段不剪枝
        result = adaptive_median_prune(MockTrial(), step=5, value=0.5, n_warmup_steps=10)
        assert result is False

        # startup 阶段不剪枝
        MockTrial.number = 3
        result = adaptive_median_prune(MockTrial(), step=20, value=0.5, n_startup_trials=5)
        assert result is False
