"""AXON HPO 单元测试。

测试范围：
- SearchSpaceDef 参数采样
- OptunaHPO 单目标/多目标
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
    """OptunaHPO 测试"""

    def test_single_objective(self):
        """测试单目标优化"""
        def objective(params):
            return [params.get("learning_rate", 0.001) * 100]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_single",
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
        def objective(params):
            lr = params.get("learning_rate", 0.001)
            gamma = params.get("gamma", 0.99)
            return [lr * 100, gamma]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_multi",
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
        def objective(params):
            return [params.get("learning_rate", 0.001) * 100]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_collect",
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

    def test_report_intermediate(self):
        """测试中间值报告"""
        def objective(params):
            lr = params.get("learning_rate", 0.001)
            for i in range(3):
                # 通过闭包访问 hpo 实例
                nonlocal hpo
                hpo.report(trial_number, i, lr * (i + 1))
            return [lr * 100]

        hpo = OptunaHPO(
            search_space=small_search_space(),
            objective_fn=objective,
            study_name="test_intermediate",
            directions="maximize",
        )
        trial_number = 0  # 用于传递 trial number

        results = hpo.run(n_trials=3, n_jobs=1)
        assert len(results) == 3

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
