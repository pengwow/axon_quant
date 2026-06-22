#!/usr/bin/env python3
"""Axon Ensemble 模块演示 —— 多模型集成投票策略。

覆盖:
  1. 三种投票策略: HardVote / SoftVote / WeightedVote
  2. EnsembleManager 注册模型与预测
  3. 模型多样性计算
  4. 权重查询

运行方式:
    source .venv/bin/activate
    python examples/14_ensemble/ensemble_demo.py
"""

from __future__ import annotations

import sys

RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
RED = "\033[31m"
GREEN = "\033[32m"
YELLOW = "\033[33m"
CYAN = "\033[36m"

if sys.platform == "win32":
    try:
        import os
        os.system("")
    except Exception:
        pass


def header(title: str, icon: str = "▶") -> None:
    print(f"\n{BOLD}{CYAN}{'═' * 60}{RESET}")
    print(f"{BOLD}{CYAN}  {icon} {title}{RESET}")
    print(f"{BOLD}{CYAN}{'═' * 60}{RESET}")


def step(n: int, text: str) -> None:
    print(f"\n  {BOLD}{YELLOW}[步骤 {n}]{RESET} {text}")


def ok(msg: str) -> None:
    print(f"    {GREEN}✅ {msg}{RESET}")


def info(msg: str) -> None:
    print(f"    {DIM}{msg}{RESET}")


def warn(msg: str) -> None:
    print(f"    {YELLOW}⚠️  {msg}{RESET}")


def fail(msg: str) -> None:
    print(f"    {RED}❌ {msg}{RESET}")


def value(label: str, v, width: int = 22) -> None:
    print(f"    {BOLD}{label:<{width}}{RESET} {v}")


def separator() -> None:
    print(f"    {DIM}{'─' * 50}{RESET}")


def demo_ensemble() -> None:
    header("Ensemble: 多模型集成投票策略", "🔗")

    from axon_quant.ensemble import (
        EnsembleManager,
        HardVoteStrategy,
        SoftVoteStrategy,
        WeightedVoteStrategy,
        ModelType,
        ActionType,
        Observation,
        Action,
    )

    step(1, "创建模拟模型函数")

    def always_buy(obs):
        return {"action_type": "buy", "symbol": "BTCUSDT", "quantity": 1.0, "confidence": 0.9}

    def always_sell(obs):
        return {"action_type": "sell", "symbol": "BTCUSDT", "quantity": 0.5, "confidence": 0.8}

    def always_hold(obs):
        return {"action_type": "hold", "symbol": "BTCUSDT", "quantity": 0.0, "confidence": 0.6}

    ok("3 个模拟模型: always_buy / always_sell / always_hold")

    step(2, "策略 1 — HardVote (硬投票)")

    hard = HardVoteStrategy()
    mgr_hard = EnsembleManager(hard)
    mgr_hard.register_model(always_buy, "buy_model", ModelType.PPO)
    mgr_hard.register_model(always_sell, "sell_model", ModelType.SAC)
    mgr_hard.register_model(always_hold, "hold_model", ModelType.RuleBased)

    obs = Observation([100.0, 50.0], [1.5, 0.8], [0.1])
    action = mgr_hard.predict(obs, 1_000_000_000)

    value("策略名", mgr_hard.strategy_name())
    value("模型数量", mgr_hard.model_count())
    value("预测动作", action.action_type)
    value("置信度", f"{action.confidence:.4f}")
    value("历史记录数", mgr_hard.history_len())
    ok("HardVote: 多数表决, 三票各 1 票平局 → 默认 Hold")

    step(3, "策略 2 — SoftVote (软投票)")

    soft = SoftVoteStrategy()
    mgr_soft = EnsembleManager(soft)
    mgr_soft.register_model(always_buy, "buy_model", ModelType.PPO)
    mgr_soft.register_model(always_sell, "sell_model", ModelType.SAC)
    mgr_soft.register_model(always_hold, "hold_model", ModelType.RuleBased)

    action2 = mgr_soft.predict(obs, 2_000_000_000)

    value("策略名", mgr_soft.strategy_name())
    value("预测动作", action2.action_type)
    value("置信度", f"{action2.confidence:.4f}")
    ok("SoftVote: 按概率加权平均, 综合所有模型概率分布")

    step(4, "策略 3 — WeightedVote (加权投票)")

    weighted = WeightedVoteStrategy([0.6, 0.3, 0.1])
    mgr_weighted = EnsembleManager(weighted)
    mgr_weighted.register_model(always_buy, "buy_model", ModelType.PPO)
    mgr_weighted.register_model(always_sell, "sell_model", ModelType.SAC)
    mgr_weighted.register_model(always_hold, "hold_model", ModelType.RuleBased)

    action3 = mgr_weighted.predict(obs, 3_000_000_000)

    value("策略名", mgr_weighted.strategy_name())
    value("权重分配", "0.6 / 0.3 / 0.1")
    value("预测动作", action3.action_type)
    value("置信度", f"{action3.confidence:.4f}")
    ok("WeightedVote: 按指定权重分配模型话语权")

    step(5, "查询模型权重")

    weights = mgr_weighted.get_weights()
    for w in weights:
        info(f"  {w.model_name}: weight={w.weight:.4f}")
    ok("权重来自投票策略内部, register_model 后自动均分")

    step(6, "计算模型多样性")

    observations = [
        Observation([100.0 + i, 50.0 + i], [1.5, 0.8], [0.1])
        for i in range(10)
    ]
    diversity = mgr_hard.compute_diversity(observations)
    value("多样性分数", f"{diversity:.4f}")
    value("含义", "0.0=完全一致, 1.0=完全分歧")

    if diversity > 0.5:
        ok(f"高多样性 ({diversity:.2f}): 模型间分歧明显")
    elif diversity > 0.0:
        warn(f"中等多样性 ({diversity:.2f}): 部分模型存在分歧")
    else:
        info("低多样性: 所有模型预测一致")

    step(7, "多步预测与历史追踪")

    mgr_step = EnsembleManager(HardVoteStrategy())
    mgr_step.register_model(always_buy, "buy_model", ModelType.PPO)
    mgr_step.register_model(always_sell, "sell_model", ModelType.SAC)

    for i in range(5):
        obs_i = Observation([100.0 + i], [1.0], [float(i)])
        act_i = mgr_step.predict(obs_i, i * 1_000_000)
        info(f"  Step {i}: {act_i.action_type}, conf={act_i.confidence:.4f}")

    value("历史记录数", mgr_step.history_len())
    ok("每步预测自动记录到历史")

    separator()
    ok("Ensemble 演示完成! 支持 HardVote / SoftVote / WeightedVote\n")


def main() -> int:
    try:
        demo_ensemble()
        return 0
    except Exception as e:
        fail(f"执行出错: {e}")
        import traceback
        traceback.print_exc()
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
