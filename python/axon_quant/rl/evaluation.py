"""RL 评估设施：通用 predict_fn 驱动 env 逐 step 回放并计算交易指标。

异构模型接入协议：``predict_fn(obs) -> action``（SB3 / ONNX / 规则模型只需
包一层适配；原 backend 版本固定调用 ``model.predict(obs, deterministic=True)``）。
设施不感知应用默认费率、模型 IO、数据加载——pass-through 原则。

2026-11-11 从 QuantCell ``backend/rl/evaluation.py`` 的 ``evaluate_model`` +
``_compute_metrics`` 下沉（spec: rl-backtest-sink-axon-quant），数值口径逐行对齐：
√252 年化（仅 len(returns)>1 且 std>0 才算）、returns 先 nan_to_num、
profit_factor 基于收益率分母（非 NAV 增量）、无亏损且盈利 999.99 哨兵、
max_drawdown_pct 带负号、total_pnl round 2 / 其余 round 4 / turnover 不 round。
"""

from __future__ import annotations

import math
from collections.abc import Callable
from typing import Any

import numpy as np

ANNUALIZATION = 252
# 无亏损场景的 profit_factor 哨兵：沿用 result_formatter_service 惯例，防 inf 泄漏 JSON
PROFIT_FACTOR_SENTINEL = 999.99
# 原 evaluate_model 的 reset 默认初始 NAV（env 未回传 portfolio_value 时）
_DEFAULT_INITIAL_NAV = 100_000.0


def compute_metrics(nav_history: list[float], num_trades: int, turnover: float = 0.0) -> dict:
    """NAV 轨迹 → 纯统计指标 dict（不含 symbol/interval/engine 等编排字段）。

    返回字段与原 ``EvaluationMetrics`` dataclass 一一对应（仅载体从 dataclass
    改为 dict，便于跨进程 JSON 传递）。
    """
    if len(nav_history) < 2:
        # 短轨迹早退：与原 evaluate_model 直接返回 EvaluationMetrics() 默认实例对齐
        return {
            "total_pnl": 0.0,
            "total_return_pct": 0.0,
            "sharpe_ratio": 0.0,
            "max_drawdown_pct": 0.0,
            "win_rate": 0.0,
            "num_trades": 0,
            "profit_factor": 0.0,
            "turnover": 0.0,
        }

    nav = np.asarray(nav_history, dtype=np.float64)
    # 原口径：per-step 收益率，nan/inf 清零（如中途 NAV 归零导致的除零），再进统计
    returns = np.diff(nav) / nav[:-1]
    returns = np.nan_to_num(returns, nan=0.0, posinf=0.0, neginf=0.0)

    total_pnl = float(nav[-1] - nav[0])
    total_return_pct = (total_pnl / nav[0] * 100) if nav[0] != 0 else 0.0

    # 原口径：至少 2 个收益点且波动为正才算 sharpe，否则 0.0（零波动轨迹不产 nan）
    if len(returns) > 1 and np.std(returns) > 0:
        sharpe_ratio = float(np.mean(returns) / np.std(returns) * math.sqrt(ANNUALIZATION))
    else:
        sharpe_ratio = 0.0

    # 原口径：回撤带负号输出；peak<=0 时分母用 1.0 兜底
    peak = np.maximum.accumulate(nav)
    drawdowns = (peak - nav) / np.where(peak > 0, peak, 1.0)
    max_drawdown_pct = float(-np.max(drawdowns) * 100)

    positive_steps = np.sum(returns > 0)
    win_rate = float(positive_steps / len(returns)) if len(returns) > 0 else 0.0

    # 原口径：profit_factor 基于收益率比值（非 NAV 增量绝对金额）
    pos_returns = returns[returns > 0]
    neg_returns = returns[returns < 0]
    if len(neg_returns) > 0 and np.sum(np.abs(neg_returns)) > 0:
        profit_factor = float(np.sum(pos_returns) / np.sum(np.abs(neg_returns)))
    else:
        profit_factor = PROFIT_FACTOR_SENTINEL if len(pos_returns) > 0 else 0.0

    return {
        "total_pnl": round(total_pnl, 2),
        "total_return_pct": round(float(total_return_pct), 4),
        "sharpe_ratio": round(sharpe_ratio, 4),
        "max_drawdown_pct": round(max_drawdown_pct, 4),
        "win_rate": round(win_rate, 4),
        "num_trades": int(num_trades),
        "profit_factor": round(profit_factor, 4),
        "turnover": float(turnover),
    }


def evaluate(
    predict_fn: Callable[[Any], Any],
    env: Any,
    transaction_cost: float | None = None,
) -> dict:
    """逐 step 驱动 env（Gymnasium 风格 5-tuple），收集 NAV 轨迹后转指标 dict。

    transaction_cost 为环境实际费率（TradingEnv 成本定义即 notional × fee_rate），
    传入时用累计 transaction_costs / fee_rate 反推成交名义总额——是恒等式不是估算；
    None 或 0 反推无效，turnover 记 0.0（backend 原口径）。
    """
    obs, reset_info = env.reset()
    initial_nav = float(reset_info.get("portfolio_value", _DEFAULT_INITIAL_NAV))
    nav_history = [initial_nav]
    num_trades = 0
    info: dict = {}
    done = False

    while not done:
        action = predict_fn(obs)
        obs, _reward, terminated, truncated, info = env.step(action)
        done = terminated or truncated
        # 原口径：portfolio_value → nav → 0.0 双回退；成交笔数取环境语义的 trades_executed
        nav = info.get("portfolio_value", info.get("nav", 0.0))
        nav_history.append(nav)
        num_trades = int(info.get("trades_executed", 0))

    if len(nav_history) < 2:
        # 原口径：早退在读取成本之前，返回全零默认指标
        return compute_metrics(nav_history, 0, turnover=0.0)

    # 原口径：TradingEnv 的 info 只暴露累计 transaction_costs，取最后一步的累计值
    transaction_costs = float(info.get("transaction_costs", 0.0))
    turnover = transaction_costs / transaction_cost if transaction_cost else 0.0

    return compute_metrics(nav_history, num_trades, turnover=turnover)
