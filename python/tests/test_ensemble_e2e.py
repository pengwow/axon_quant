"""axon_quant.ensemble 端到端测试(L3 Python E2E)。

覆盖范围:
1. 类型导入 / 实例化
2. ModelType / ActionType / EnsembleStrategy 枚举
3. ActionProbabilities 创建和归一化
4. Action / Observation 创建
5. HardVoteStrategy / SoftVoteStrategy / WeightedVoteStrategy 投票
6. EnsembleManager 注册模型和预测
7. MetaModel / StackingEnsemble 堆叠集成
8. 异常路径(EnsembleError)

运行::

    cd /Users/liupeng/workspace/quant/axon
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon/.venv/bin/python \\
        python -m pytest python/tests/test_ensemble_e2e.py -v

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
    from axon_quant.ensemble import (
        Action,
        ActionProbabilities,
        ActionType,
        EnsembleError,
        EnsembleManager,
        EnsembleStrategy,
        HardVoteStrategy,
        MetaModel,
        ModelType,
        ModelWeight,
        Observation,
        SoftVoteStrategy,
        StackingEnsemble,
        WeightedVoteStrategy,
    )
    _ENSEMBLE_AVAILABLE = hasattr(axon_quant, "_native") and hasattr(
        axon_quant._native, "ensemble"
    )
except ImportError as _e:
    pytest.skip(f"axon_quant not installed: {_e}", allow_module_level=True)
    raise

if not _ENSEMBLE_AVAILABLE:
    pytest.skip(
        "axon_quant._native.ensemble not yet registered (need maturin develop)",
        allow_module_level=True,
    )


# ═══════════════════════════════════════════════════════════════════════════
# 类型可用性
# ═══════════════════════════════════════════════════════════════════════════


def test_ensemble_module_imports_all_symbols():
    """所有 ensemble 顶层符号都能 import。"""
    assert ModelType is not None
    assert ActionType is not None
    assert EnsembleStrategy is not None
    assert ActionProbabilities is not None
    assert Action is not None
    assert Observation is not None
    assert ModelWeight is not None
    assert HardVoteStrategy is not None
    assert SoftVoteStrategy is not None
    assert WeightedVoteStrategy is not None
    assert EnsembleManager is not None
    assert MetaModel is not None
    assert StackingEnsemble is not None
    assert EnsembleError is not None


# ═══════════════════════════════════════════════════════════════════════════
# 枚举类型
# ═══════════════════════════════════════════════════════════════════════════


def test_model_type_values():
    """ModelType 有 5 个变体。"""
    assert ModelType.PPO is not None
    assert ModelType.SAC is not None
    assert ModelType.DQN is not None
    assert ModelType.A2C is not None
    assert ModelType.RuleBased is not None


def test_action_type_values():
    """ActionType 有 3 个变体。"""
    assert ActionType.Buy is not None
    assert ActionType.Sell is not None
    assert ActionType.Hold is not None


def test_ensemble_strategy_values():
    """EnsembleStrategy 有 5 个变体。"""
    assert EnsembleStrategy.HardVote is not None
    assert EnsembleStrategy.SoftVote is not None
    assert EnsembleStrategy.WeightedVote is not None
    assert EnsembleStrategy.Stacking is not None
    assert EnsembleStrategy.DynamicWeighted is not None


def test_action_type_str():
    """ActionType.__str__ 返回小写字符串。"""
    assert str(ActionType.Buy) == "buy"
    assert str(ActionType.Sell) == "sell"
    assert str(ActionType.Hold) == "hold"


# ═══════════════════════════════════════════════════════════════════════════
# ActionProbabilities
# ═══════════════════════════════════════════════════════════════════════════


def test_action_probabilities_creation():
    """ActionProbabilities 自动归一化。"""
    probs = ActionProbabilities(2.0, 1.0, 1.0)
    assert probs.buy == 0.5
    assert probs.sell == 0.25
    assert probs.hold == 0.25


def test_action_probabilities_to_list():
    """ActionProbabilities.to_list() 返回 [buy, sell, hold]。"""
    probs = ActionProbabilities(1.0, 1.0, 1.0)
    lst = probs.to_list()
    assert len(lst) == 3
    assert lst[0] == pytest.approx(1.0 / 3.0)
    assert lst[1] == pytest.approx(1.0 / 3.0)
    assert lst[2] == pytest.approx(1.0 / 3.0)


def test_action_probabilities_zero_total():
    """ActionProbabilities 全零时使用均匀分布。"""
    probs = ActionProbabilities(0.0, 0.0, 0.0)
    assert probs.buy == pytest.approx(1.0 / 3.0)
    assert probs.sell == pytest.approx(1.0 / 3.0)
    assert probs.hold == pytest.approx(1.0 / 3.0)


# ═══════════════════════════════════════════════════════════════════════════
# Action
# ═══════════════════════════════════════════════════════════════════════════


def test_action_creation():
    """Action 可以创建并访问属性。"""
    action = Action(ActionType.Buy, "BTCUSDT", 1.0, 0.8)
    assert action.action_type == ActionType.Buy
    assert action.symbol == "BTCUSDT"
    assert action.quantity == 1.0
    assert action.confidence == 0.8


def test_action_to_dict():
    """Action.to_dict() 返回正确字段。"""
    action = Action(ActionType.Sell, "ETHUSDT", 0.5, 0.7)
    d = action.to_dict()
    assert d["action_type"] == "sell"
    assert d["symbol"] == "ETHUSDT"
    assert d["quantity"] == 0.5
    assert d["confidence"] == 0.7


# ═══════════════════════════════════════════════════════════════════════════
# Observation
# ═══════════════════════════════════════════════════════════════════════════


def test_observation_creation():
    """Observation 可以创建并访问属性。"""
    obs = Observation([1.0, 2.0], [3.0, 4.0], [5.0])
    assert obs.market_features == [1.0, 2.0]
    assert obs.technical_indicators == [3.0, 4.0]
    assert obs.time_features == [5.0]


def test_observation_to_dict():
    """Observation.to_dict() 返回正确字段。"""
    obs = Observation([1.0], [2.0], [3.0])
    d = obs.to_dict()
    assert d["market_features"] == [1.0]
    assert d["technical_indicators"] == [2.0]
    assert d["time_features"] == [3.0]


# ═══════════════════════════════════════════════════════════════════════════
# 投票策略
# ═══════════════════════════════════════════════════════════════════════════


def test_hard_vote_strategy_creation():
    """HardVoteStrategy 可以创建。"""
    strategy = HardVoteStrategy()
    assert strategy is not None
    assert "HardVoteStrategy" in repr(strategy)


def test_soft_vote_strategy_creation():
    """SoftVoteStrategy 可以创建。"""
    strategy = SoftVoteStrategy()
    assert strategy is not None
    assert "SoftVoteStrategy" in repr(strategy)


def test_weighted_vote_strategy_creation():
    """WeightedVoteStrategy 可以创建。"""
    strategy = WeightedVoteStrategy([0.5, 0.3, 0.2])
    assert strategy is not None


def test_weighted_vote_strategy_uniform():
    """WeightedVoteStrategy.uniform() 创建均匀权重。"""
    strategy = WeightedVoteStrategy.uniform(3)
    assert strategy is not None


def test_weighted_vote_strategy_invalid_weights():
    """WeightedVoteStrategy 权重和不为 1 抛异常。"""
    with pytest.raises(EnsembleError):
        WeightedVoteStrategy([0.5, 0.3, 0.1])  # sum = 0.9


# ═══════════════════════════════════════════════════════════════════════════
# EnsembleManager
# ═══════════════════════════════════════════════════════════════════════════


def _buy_model(obs):
    """总是买入的模型。"""
    return {"action_type": "buy", "symbol": "BTCUSDT", "quantity": 1.0, "confidence": 0.9}


def _sell_model(obs):
    """总是卖出的模型。"""
    return {"action_type": "sell", "symbol": "BTCUSDT", "quantity": 0.5, "confidence": 0.8}


def test_ensemble_manager_creation():
    """EnsembleManager 可以创建。"""
    mgr = EnsembleManager(HardVoteStrategy())
    assert mgr is not None
    assert mgr.model_count() == 0
    assert "EnsembleManager" in repr(mgr)


def test_ensemble_manager_register_model():
    """EnsembleManager 注册模型后计数增加。"""
    mgr = EnsembleManager(HardVoteStrategy())
    mgr.register_model(_buy_model, "buy_model", ModelType.RuleBased)
    assert mgr.model_count() == 1


def test_ensemble_manager_predict():
    """EnsembleManager.predict() 返回 Action。"""
    mgr = EnsembleManager(HardVoteStrategy())
    mgr.register_model(_buy_model, "buy_model", ModelType.RuleBased)
    obs = Observation([1.0], [2.0], [3.0])
    action = mgr.predict(obs, 1000)
    assert action is not None
    assert action.action_type == ActionType.Buy


def test_ensemble_manager_get_weights():
    """EnsembleManager.get_weights() 返回权重列表。"""
    mgr = EnsembleManager(HardVoteStrategy())
    mgr.register_model(_buy_model, "m1", ModelType.RuleBased)
    mgr.register_model(_sell_model, "m2", ModelType.RuleBased)
    weights = mgr.get_weights()
    assert len(weights) == 2
    assert weights[0].weight == pytest.approx(0.5)
    assert weights[1].weight == pytest.approx(0.5)


def test_ensemble_manager_strategy_name():
    """EnsembleManager.strategy_name() 返回策略名。"""
    mgr = EnsembleManager(HardVoteStrategy())
    assert mgr.strategy_name() == "hard_vote"


# ═══════════════════════════════════════════════════════════════════════════
# MetaModel / StackingEnsemble
# ═══════════════════════════════════════════════════════════════════════════


def test_meta_model_creation():
    """MetaModel 可以创建。"""
    model = MetaModel(10, 3)
    assert model is not None
    assert "MetaModel" in repr(model)


def test_meta_model_with_weights():
    """MetaModel.with_weights() 创建带权重的模型。"""
    weights = [[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]]
    bias = [0.0, 0.0, 0.0]
    model = MetaModel.with_weights(weights, bias)
    assert model is not None


def test_stacking_ensemble_creation():
    """StackingEnsemble 可以创建。"""
    meta = MetaModel(3, 3)
    models = [
        (_buy_model, "buy_model", ModelType.RuleBased),
    ]
    ensemble = StackingEnsemble(models, meta)
    assert ensemble is not None
    assert ensemble.base_model_count() == 1


def test_stacking_ensemble_predict():
    """StackingEnsemble.predict() 返回 Action。"""
    meta = MetaModel(3, 3)
    models = [
        (_buy_model, "buy_model", ModelType.RuleBased),
    ]
    ensemble = StackingEnsemble(models, meta)
    obs = Observation([1.0], [2.0], [3.0])
    action = ensemble.predict(obs)
    assert action is not None
    assert action.action_type in (ActionType.Buy, ActionType.Sell, ActionType.Hold)
