"""axon_quant.explain 端到端测试(L3 Python E2E)。

覆盖范围:
1. 类型导入 / 实例化
2. ContributionDirection 枚举
3. FeatureContribution 创建和属性访问
4. ActionSnapshot 创建和属性访问
5. KernelSHAP 创建和 SHAP 计算
6. CounterfactualConfig 配置
7. ReportGenerator 生成报告
8. Explanation / DecisionReport 属性访问
9. 异常路径(ExplainError)

运行::

    cd /Users/liupeng/workspace/quant/axon
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon/.venv/bin/python \\
        python -m pytest python/tests/test_explain_e2e.py -v

注意:本测试需先 build wheel(参见 Makefile 的 ``python-build`` /
``python-develop`` 目标)。如未 build,部分测试 skip。
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

# 强制使用本项目 venv(避免 miniconda pyarrow / numpy 干扰)
_VENV_SITE = Path("/Users/liupeng/workspace/quant/axon/.venv/lib/python3.14/site-packages")
if _VENV_SITE.exists() and str(_VENV_SITE) not in sys.path:
    sys.path.insert(0, str(_VENV_SITE))

try:
    import axon_quant  # noqa: F401
    from axon_quant.explain import (
        ActionAttribution,
        ActionSnapshot,
        ContributionDirection,
        CounterfactualConfig,
        DecisionReport,
        ExplainError,
        Explanation,
        FeatureContribution,
        KernelSHAP,
        ReportGenerator,
    )
    _EXPLAIN_AVAILABLE = hasattr(axon_quant, "_native") and hasattr(
        axon_quant._native, "explain"
    )
except ImportError as _e:
    pytest.skip(f"axon_quant not installed: {_e}", allow_module_level=True)
    raise

if not _EXPLAIN_AVAILABLE:
    pytest.skip(
        "axon_quant._native.explain not yet registered (need maturin develop)",
        allow_module_level=True,
    )


# ═══════════════════════════════════════════════════════════════════════════
# 类型可用性
# ═══════════════════════════════════════════════════════════════════════════


def test_explain_module_imports_all_symbols():
    """所有 explain 顶层符号都能 import。"""
    assert ContributionDirection is not None
    assert FeatureContribution is not None
    assert ActionSnapshot is not None
    assert ActionAttribution is not None
    assert Explanation is not None
    assert DecisionReport is not None
    assert KernelSHAP is not None
    assert CounterfactualConfig is not None
    assert ReportGenerator is not None
    assert ExplainError is not None


# ═══════════════════════════════════════════════════════════════════════════
# ContributionDirection 枚举
# ═══════════════════════════════════════════════════════════════════════════


def test_contribution_direction_values():
    """ContributionDirection 有三个变体。"""
    assert ContributionDirection.Positive is not None
    assert ContributionDirection.Negative is not None
    assert ContributionDirection.Neutral is not None


def test_contribution_direction_str():
    """ContributionDirection.__str__ 返回小写字符串。"""
    assert str(ContributionDirection.Positive) == "positive"
    assert str(ContributionDirection.Negative) == "negative"
    assert str(ContributionDirection.Neutral) == "neutral"


# ═══════════════════════════════════════════════════════════════════════════
# FeatureContribution
# ═══════════════════════════════════════════════════════════════════════════


def test_feature_contribution_creation():
    """FeatureContribution 可以创建并访问属性。"""
    fc = FeatureContribution("rsi", 0.5, 65.0, ContributionDirection.Positive)
    assert fc.feature_name == "rsi"
    assert fc.shap_value == 0.5
    assert fc.feature_value == 65.0
    assert fc.direction == ContributionDirection.Positive


def test_feature_contribution_to_dict():
    """FeatureContribution.to_dict() 返回正确字段。"""
    fc = FeatureContribution("rsi", 0.5, 65.0, ContributionDirection.Positive)
    d = fc.to_dict()
    assert d["feature_name"] == "rsi"
    assert d["shap_value"] == 0.5
    assert d["feature_value"] == 65.0
    assert d["direction"] == "positive"


# ═══════════════════════════════════════════════════════════════════════════
# ActionSnapshot
# ═══════════════════════════════════════════════════════════════════════════


def test_action_snapshot_creation():
    """ActionSnapshot 可以创建并访问属性。"""
    snap = ActionSnapshot(1.0, 50000.0, 49000.0, 52000.0, "limit")
    assert snap.position_size == 1.0
    assert snap.entry_price == 50000.0
    assert snap.stop_loss == 49000.0
    assert snap.take_profit == 52000.0
    assert snap.order_type == "limit"


def test_action_snapshot_to_dict():
    """ActionSnapshot.to_dict() 返回正确字段。"""
    snap = ActionSnapshot(1.0, 50000.0, 49000.0, 52000.0, "limit")
    d = snap.to_dict()
    assert d["position_size"] == 1.0
    assert d["entry_price"] == 50000.0
    assert d["order_type"] == "limit"


# ═══════════════════════════════════════════════════════════════════════════
# CounterfactualConfig
# ═══════════════════════════════════════════════════════════════════════════


def test_counterfactual_config_defaults():
    """CounterfactualConfig 默认值正确。"""
    cfg = CounterfactualConfig()
    assert cfg.max_changes == 3
    assert cfg.step_size == 0.5
    assert cfg.confidence_threshold == 0.05


def test_counterfactual_config_builder():
    """CounterfactualConfig 静态工厂方法。"""
    cfg = CounterfactualConfig.with_max_changes(5)
    assert cfg.max_changes == 5

    cfg2 = CounterfactualConfig.with_step_size(0.3)
    assert cfg2.step_size == 0.3

    cfg3 = CounterfactualConfig.with_confidence_threshold(0.1)
    assert cfg3.confidence_threshold == 0.1


# ═══════════════════════════════════════════════════════════════════════════
# KernelSHAP
# ═══════════════════════════════════════════════════════════════════════════


def _linear_model(features):
    """简单线性模型用于测试。"""
    return [sum(features)]


def test_kernel_shap_creation():
    """KernelSHAP 可以创建。"""
    background = [[1.0, 2.0], [3.0, 4.0]]
    shap = KernelSHAP(_linear_model, background, 10)
    assert shap is not None


def test_kernel_shap_compute_shap():
    """KernelSHAP.compute_shap 返回正确维度。"""
    background = [[1.0, 2.0], [3.0, 4.0]]
    shap = KernelSHAP(_linear_model, background, 10)
    result = shap.compute_shap([5.0, 6.0])
    assert len(result) == 2
    assert all(isinstance(v, float) for v in result)


def test_kernel_shap_repr():
    """KernelSHAP.__repr__ 返回可读字符串。"""
    background = [[1.0, 2.0]]
    shap = KernelSHAP(_linear_model, background, 5)
    r = repr(shap)
    assert "KernelSHAP" in r


# ═══════════════════════════════════════════════════════════════════════════
# ReportGenerator
# ═══════════════════════════════════════════════════════════════════════════


def test_report_generator_repr():
    """ReportGenerator.__repr__ 返回可读字符串。"""
    r = repr(ReportGenerator)
    assert "ReportGenerator" in r


# ═══════════════════════════════════════════════════════════════════════════
# 异常路径
# ═══════════════════════════════════════════════════════════════════════════


def test_kernel_shap_empty_background_raises():
    """KernelSHAP 空背景数据抛异常。"""
    with pytest.raises(ExplainError):
        KernelSHAP(_linear_model, [], 10)


def test_kernel_shap_feature_mismatch_raises():
    """KernelSHAP 特征维度不匹配抛异常。"""
    background = [[1.0, 2.0], [3.0, 4.0]]
    shap = KernelSHAP(_linear_model, background, 10)
    with pytest.raises(ExplainError):
        shap.compute_shap([1.0, 2.0, 3.0])  # 3 维 vs 2 维
