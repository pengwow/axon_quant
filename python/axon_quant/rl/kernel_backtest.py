"""RL 内核回测设施 — worker.Kernel + RL host 的一段式执行包装。

2026-11-11 从 QuantCell ``backend/services/rl_service.py::_run_kernel_backtest``
的执行段下沉（spec: rl-backtest-sink-axon-quant）。应用层保留模型 IO / 数据
清洗 / symbol 归一化；费率等口径全部 pass-through，设施不内置任何应用默认数字。

不提供 slippage 参数：worker.Kernel 的 RL 回测面只有 fee_rate，
slippage 是 TradingEnv/训练侧概念。
"""

from __future__ import annotations

from collections.abc import Callable, Mapping, Sequence
from typing import Any

# worker 由 axon_quant.__init__ 的 `from ._native import *` 属性注入,
# 不是文件系统子模块,必须用 from-import 属性解析(test_kernel_rl.py 同款)
from .. import worker  # noqa: E402


def run_kernel_backtest(
    bars: Sequence[Mapping],
    predict_fn: Callable[[Any], Any],
    symbol: str,
    interval: str,
    spec: Mapping[str, Any],
    initial_capital: float = 100_000.0,
    fee_rate: float = 0.0,
    stats: Any | None = None,
    model_id: str = "rl_model",
) -> dict:
    """bars + predict_fn → 统一内核 RL 回测，返回 15 字段响应 dict。

    spec 四键（与 add_rl_strategy 形参对齐）：spec_version / window_size /
    features / action_space。stats 为观测统计（RunningStats 实例，原样透传）。
    """
    kernel = worker.Kernel(
        initial_cash=initial_capital, interval=interval, fee_rate=fee_rate,
    )
    kernel.add_rl_strategy(
        predict_fn,
        model_id,
        spec["spec_version"],
        spec["window_size"],
        spec["features"],
        spec["action_space"],
        symbol,
        stats=stats,
    )
    result = kernel.run(bars_by_symbol={symbol: [dict(b) for b in bars]})

    # 15 字段契约与 backend 原实现逐字段一致（含 profit_factor=None、
    # turnover=fills_detail 累加 Σ|price×qty|、缺失置 None）
    fills_detail = getattr(result, "fills_detail", None)
    turnover = (
        sum(abs(float(f["price"]) * float(f["quantity"])) for f in fills_detail)
        if fills_detail is not None
        else None
    )
    total_pnl = float(result.total_pnl)
    return {
        "symbol": symbol,                       # 回显入参；对外口径由调用方覆盖
        "interval": interval,
        "data_bars": len(bars),                 # 以传入 bars 计数；对外口径由调用方覆盖
        "initial_capital": initial_capital,
        "total_pnl": total_pnl,
        "total_return_pct": (
            round(total_pnl / initial_capital * 100, 4) if initial_capital else 0.0
        ),
        "sharpe_ratio": float(result.sharpe_ratio),
        "max_drawdown_pct": float(result.max_drawdown_pct) * 100.0,
        "win_rate": float(result.win_rate),
        "num_trades": int(result.fills),
        "profit_factor": None,
        "total_fees": float(result.total_fees),
        "total_funding": float(result.total_funding_pnl),
        "turnover": turnover,
        "engine": "kernel",
    }
