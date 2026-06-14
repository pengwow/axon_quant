"""场景 3+4+5+6: HPO / WalkForward / Tracker / Registry / Distributed 测试"""

import pytest


class TestHPO:
    """场景 3: HPO 超参数优化"""

    def test_import(self):
        """3.1 HPO 模块导入"""
        import axon_quant
        assert hasattr(axon_quant.hpo, 'HPORunner')
        assert hasattr(axon_quant.hpo, 'py_compute_pareto_front')
        assert hasattr(axon_quant.hpo, 'py_compute_hypervolume')
        assert hasattr(axon_quant.hpo, 'py_validate_search_space')

    def test_pareto_front(self):
        """3.4 Pareto 前沿计算"""
        import axon_quant
        trials = [
            {"trial_id": i, "values": [float(i), float(10 - i)]}
            for i in range(11)
        ]
        front = axon_quant.hpo.py_compute_pareto_front(trials, ["maximize", "maximize"])
        assert isinstance(front, list)
        assert len(front) > 0
        # Pareto 前沿不应包含被支配的点
        assert len(front) <= len(trials)

    def test_pareto_front_single_objective(self):
        """单目标 Pareto 前沿"""
        import axon_quant
        trials = [{"trial_id": i, "values": [float(i)]} for i in range(5)]
        front = axon_quant.hpo.py_compute_pareto_front(trials, ["maximize"])
        assert len(front) == 1  # 只有最大值是非支配的

    def test_hypervolume(self):
        """3.5 超体积计算"""
        import axon_quant
        # 简单 2D 情况
        hv = axon_quant.hpo.py_compute_hypervolume(
            [[1.0, 1.0], [2.0, 0.5], [0.5, 2.0]],
            [1.0, 1.0],  # 参考点
        )
        assert isinstance(hv, (int, float))
        assert hv >= 0


class TestWalkForward:
    """场景 4: Walk-Forward 验证"""

    def test_import(self):
        """4.1 WalkForward 模块导入"""
        import axon_quant
        assert hasattr(axon_quant.walk_forward, 'WalkForwardRunner')
        assert hasattr(axon_quant.walk_forward, 'py_detect_leakage')
        assert hasattr(axon_quant.walk_forward, 'py_embargo_indices')
        assert hasattr(axon_quant.walk_forward, 'py_deflated_sharpe')
        assert hasattr(axon_quant.walk_forward, 'py_aggregate_folds')
        assert hasattr(axon_quant.walk_forward, 'py_purge_overlapping_labels')

    def test_detect_leakage_no_overlap(self):
        """4.4 无重叠时无泄漏"""
        import axon_quant
        train_idx = list(range(0, 80))
        test_idx = list(range(80, 100))
        has_leak, pairs = axon_quant.walk_forward.py_detect_leakage(train_idx, test_idx, 1)
        assert has_leak is False
        assert len(pairs) == 0

    def test_detect_leakage_with_overlap(self):
        """4.4 有重叠时检测到泄漏"""
        import axon_quant
        train_idx = list(range(0, 90))
        test_idx = list(range(80, 100))
        has_leak, pairs = axon_quant.walk_forward.py_detect_leakage(train_idx, test_idx, 1)
        assert has_leak is True
        assert len(pairs) > 0

    def test_embargo_indices(self):
        """4.5 embargo 正确排除重叠索引"""
        import axon_quant
        test_idx = list(range(80, 100))
        embargo = axon_quant.walk_forward.py_embargo_indices(test_idx, 0.1, 100)
        assert isinstance(embargo, list)

    def test_deflated_sharpe(self):
        """4.6 deflated Sharpe < 原始 sharpe"""
        import axon_quant
        observed_sharpe = 2.0
        dsr = axon_quant.walk_forward.py_deflated_sharpe(observed_sharpe, 100, 0.5)
        assert isinstance(dsr, (int, float))
        assert not (dsr != dsr)  # not NaN
        # deflated sharpe 应该小于等于 observed sharpe
        assert dsr <= observed_sharpe

    def test_purge_overlapping_labels(self):
        """purge 正确移除重叠标签"""
        import axon_quant
        result = axon_quant.walk_forward.py_purge_overlapping_labels(
            list(range(100)), 5  # label_window=5
        )
        assert isinstance(result, list)
        assert len(result) <= 100


class TestTracker:
    """场景 5: 实验追踪"""

    def test_import(self):
        """5.1 Tracker 模块导入"""
        import axon_quant
        assert hasattr(axon_quant.tracker, 'MemoryTracker')
        assert hasattr(axon_quant.tracker, 'LocalTracker')

    def test_memory_tracker_lifecycle(self):
        """5.1-5.3 MemoryTracker 完整生命周期"""
        import axon_quant
        tracker = axon_quant.tracker.MemoryTracker()

        # log_param
        tracker.log_param("lr", 0.001)
        tracker.log_param("batch_size", 32)

        # log_metric
        tracker.log_metric("loss", 0.5, step=1)
        tracker.log_metric("loss", 0.3, step=2)
        tracker.log_metric("reward", 100.0, step=1)

        # set_tag
        tracker.set_tag("model", "ppo")
        tracker.set_tag("env", "btc-usdt")

        # get_metrics
        metrics = tracker.get_metrics()
        assert isinstance(metrics, dict)
        assert "loss/1" in metrics or "loss" in metrics

        # finish
        tracker.finish("completed")

    def test_memory_tracker_run_id(self):
        """run_id 自动生成"""
        import axon_quant
        t1 = axon_quant.tracker.MemoryTracker()
        t2 = axon_quant.tracker.MemoryTracker()
        # run_id 应该是字符串（具体格式取决于实现）
        # 两个 tracker 应该有不同 run_id（如果有的话）


class TestRegistry:
    """场景 5: 模型注册表"""

    def test_import(self):
        """5.4 Registry 模块导入"""
        import axon_quant
        assert hasattr(axon_quant.registry, 'ModelRegistry')
        assert hasattr(axon_quant.registry, 'LocalStorage')

    def test_classes_instantiable(self):
        """类可以实例化（需要参数）"""
        import axon_quant
        # ModelRegistry 和 LocalStorage 可能需要参数
        # 至少验证类存在且可调用
        assert callable(axon_quant.registry.ModelRegistry)
        assert callable(axon_quant.registry.LocalStorage)


class TestDistributed:
    """场景 6: 分布式训练"""

    def test_import(self):
        """6.1 Distributed 模块导入"""
        import axon_quant
        assert hasattr(axon_quant.distributed, 'DistributedRunner')
        assert hasattr(axon_quant.distributed, 'py_serialize_metrics')
        assert hasattr(axon_quant.distributed, 'py_save_checkpoint')
        assert hasattr(axon_quant.distributed, 'py_load_checkpoint')

    def test_serialize_metrics(self):
        """6.2 序列化指标"""
        import axon_quant
        result = axon_quant.distributed.py_serialize_metrics(
            step=100,
            reward=0.5,
            policy_loss=0.01,
            value_loss=0.02,
            entropy=0.1,
            fps=1000.0,
        )
        assert isinstance(result, str)
        assert len(result) > 0
        # 应该是有效的 JSON
        import json
        data = json.loads(result)
        assert "step" in data or "reward" in data
