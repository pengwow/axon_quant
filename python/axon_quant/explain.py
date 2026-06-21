"""axon_quant.explain 顶层 Python API —— thin wrapper 模式。

用法::

    from axon_quant.explain import (
        KernelSHAP, CounterfactualConfig, ReportGenerator,
        ContributionDirection, FeatureContribution, ActionSnapshot,
        Explanation, DecisionReport, ExplainError,
    )
"""

from __future__ import annotations

from axon_quant._native import explain as _native_explain_module  # noqa: E402

# 重新导出原生符号
ContributionDirection = _native_explain_module.ContributionDirection
FeatureContribution = _native_explain_module.FeatureContribution
ActionSnapshot = _native_explain_module.ActionSnapshot
ActionAttribution = _native_explain_module.ActionAttribution
CounterfactualExplanation = _native_explain_module.CounterfactualExplanation
Explanation = _native_explain_module.Explanation
DecisionReport = _native_explain_module.DecisionReport
KernelSHAP = _native_explain_module.KernelSHAP
CounterfactualConfig = _native_explain_module.CounterfactualConfig
ReportGenerator = _native_explain_module.ReportGenerator
ExplainError = _native_explain_module.ExplainError

__all__ = [
    "ContributionDirection",
    "FeatureContribution",
    "ActionSnapshot",
    "ActionAttribution",
    "CounterfactualExplanation",
    "Explanation",
    "DecisionReport",
    "KernelSHAP",
    "CounterfactualConfig",
    "ReportGenerator",
    "ExplainError",
]
