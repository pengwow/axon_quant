"""axon_quant.ensemble 顶层 Python API —— thin wrapper 模式。

用法::

    from axon_quant.ensemble import (
        EnsembleManager, HardVoteStrategy, SoftVoteStrategy,
        WeightedVoteStrategy, MetaModel, StackingEnsemble,
        ModelType, ActionType, EnsembleStrategy,
        ActionProbabilities, Action, Observation,
        EnsembleError,
    )
"""

from __future__ import annotations

from axon_quant._native import ensemble as _native_ensemble_module  # noqa: E402

# 重新导出原生符号
ModelType = _native_ensemble_module.ModelType
ActionType = _native_ensemble_module.ActionType
EnsembleStrategy = _native_ensemble_module.EnsembleStrategy
ActionProbabilities = _native_ensemble_module.ActionProbabilities
Action = _native_ensemble_module.Action
Observation = _native_ensemble_module.Observation
ModelWeight = _native_ensemble_module.ModelWeight
HardVoteStrategy = _native_ensemble_module.HardVoteStrategy
SoftVoteStrategy = _native_ensemble_module.SoftVoteStrategy
WeightedVoteStrategy = _native_ensemble_module.WeightedVoteStrategy
EnsembleManager = _native_ensemble_module.EnsembleManager
MetaModel = _native_ensemble_module.MetaModel
StackingEnsemble = _native_ensemble_module.StackingEnsemble
EnsembleError = _native_ensemble_module.EnsembleError

__all__ = [
    "ModelType",
    "ActionType",
    "EnsembleStrategy",
    "ActionProbabilities",
    "Action",
    "Observation",
    "ModelWeight",
    "HardVoteStrategy",
    "SoftVoteStrategy",
    "WeightedVoteStrategy",
    "EnsembleManager",
    "MetaModel",
    "StackingEnsemble",
    "EnsembleError",
]
