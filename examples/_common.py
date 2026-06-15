"""AXON 强化学习示例的共享工具。

提供：
- `make_synthetic_market_data(n, start=100.0, vol=0.01, seed=0)`：生成
  合成 K 线（随机游走 + 高斯噪声），无需外部数据文件。
- `make_env_config(initial_capital=100_000.0, max_steps=200, ...)`：构造
  环境配置字典。
- `make_env(config, market_data, reward="pnl")`：用合成数据构造一个
  `axon_quant.rl.TradingEnv` 实例。
- `run_random_episode(env, max_steps=100, seed=0)`：在环境中执行随机策略。
- `set_seed(seed)`：统一设置 `random` / `numpy` / `torch`（若可用）种子。

设计原则：
- **零外部依赖**：`make_synthetic_market_data` 不依赖任何第三方库。
- **使用 axon_quant**：所有 Rust 扩展通过 `axon_quant` 包调用，
  不再需要从 `target/` 目录加载共享库。
"""

from __future__ import annotations

import os
import random
from typing import Any, Iterable


# ──────────────────────────────────────────────
# 数据生成
# ──────────────────────────────────────────────


def make_synthetic_market_data(
    n: int = 500,
    start_price: float = 100.0,
    vol: float = 0.01,
    seed: int = 42,
) -> list[dict[str, Any]]:
    """生成 n 根合成 K 线（几何布朗运动）。

    Args:
        n: K 线数量
        start_price: 起始价
        vol: 日波动率（每步高斯噪声标准差）
        seed: 随机种子（可复现）

    Returns:
        list[dict]，每根 K 线含 `timestamp` / `open` / `high` / `low` /
        `close` / `volume`，符合 `axon_quant.rl.TradingEnv` 期望格式。
    """
    rng = random.Random(seed)
    bars: list[dict[str, Any]] = []
    price = start_price
    for t in range(n):
        open_ = price
        ret = rng.gauss(0.0, vol)
        close = max(1e-6, open_ * (1.0 + ret))
        spread = abs(close - open_) + open_ * vol * 0.5
        high = max(open_, close) + spread * rng.random()
        low = max(1e-6, min(open_, close) - spread * rng.random())
        volume = 1000.0 + 200.0 * rng.gauss(0.0, 1.0)
        bars.append(
            {
                "timestamp": t,
                "open": open_,
                "high": high,
                "low": low,
                "close": close,
                "volume": abs(volume),
            }
        )
        price = close
    return bars


# ──────────────────────────────────────────────
# 环境构造
# ──────────────────────────────────────────────


def make_env_config(
    initial_capital: float = 100_000.0,
    transaction_cost: float = 0.001,
    slippage: float = 0.0001,
    max_steps: int = 500,
    seed: int = 42,
    symbol: str = "BTCUSDT",
    return_window: int = 50,
) -> dict[str, Any]:
    """构造环境配置字典。"""
    return {
        "initial_capital": initial_capital,
        "transaction_cost": transaction_cost,
        "slippage": slippage,
        "max_steps": max_steps,
        "seed": seed,
        "symbol": symbol,
        "return_window": return_window,
    }


def make_env(
    config: dict[str, Any] | None = None,
    market_data: list[dict[str, Any]] | None = None,
    reward: str = "pnl",
    action_space: dict[str, Any] | None = None,
):
    """构造 `axon_quant.rl.TradingEnv` 实例。

    Args:
        config: 环境配置；None 表示使用默认
        market_data: 行情 K 线；None 时自动生成 500 根合成数据
        reward: 奖励函数名（"pnl" / "sharpe" / "sortino"）
        action_space: 动作空间定义；None 表示默认连续 `[-1, 1]`

    Returns:
        `axon_quant.rl.TradingEnv` 实例
    """
    import axon_quant  # noqa: PLC0415

    cfg = config if config is not None else make_env_config()
    data = market_data if market_data is not None else make_synthetic_market_data()
    return axon_quant.rl.TradingEnv(
        config=cfg,
        action_space=action_space,
        market_data=data,
        reward=reward,
    )


# ──────────────────────────────────────────────
# 训练辅助
# ──────────────────────────────────────────────


def set_seed(seed: int = 0) -> None:
    """统一设置 `random` / `numpy` / `torch`（若可用）种子。"""
    random.seed(seed)
    os.environ["PYTHONHASHSEED"] = str(seed)
    try:
        import numpy as np  # noqa: PLC0415

        np.random.seed(seed)
    except ImportError:
        pass
    try:
        import torch  # noqa: PLC0415

        torch.manual_seed(seed)
        if torch.cuda.is_available():
            torch.cuda.manual_seed_all(seed)
    except ImportError:
        pass


def run_random_episode(env, max_steps: int = 100, seed: int = 0) -> dict[str, Any]:
    """在环境中执行一个 episode 的随机策略（基线）。

    Args:
        env: `axon_quant.rl.TradingEnv` 实例
        max_steps: 最大步数（防止卡死）
        seed: 随机种子

    Returns:
        dict 含 `total_reward` / `steps` / `final_value` / `trades` / `done`
    """
    rng = random.Random(seed)
    env.reset()
    total_reward = 0.0
    steps = 0
    last_info: dict[str, Any] = {}
    done = False
    while not done and steps < max_steps:
        action = [rng.uniform(-1.0, 1.0)]
        result = env.step(action)
        obs_dict, reward, terminated, truncated, info = result
        total_reward += reward
        steps += 1
        last_info = info
        done = bool(terminated) or bool(truncated)

    return {
        "total_reward": total_reward,
        "steps": steps,
        "final_value": float(last_info.get("portfolio_value", 0.0)),
        "trades": int(last_info.get("trades_executed", 0)),
        "done": done,
    }


def summarize(records: Iterable[dict[str, Any]]) -> dict[str, float]:
    """聚合一组 run 记录，返回均值与样本数。"""
    records = list(records)
    if not records:
        return {"n": 0, "mean_reward": 0.0, "mean_steps": 0.0}
    n = len(records)
    return {
        "n": float(n),
        "mean_reward": sum(r["total_reward"] for r in records) / n,
        "mean_steps": sum(r["steps"] for r in records) / n,
        "mean_final_value": sum(r["final_value"] for r in records) / n,
    }


__all__ = [
    "make_env",
    "make_env_config",
    "make_synthetic_market_data",
    "run_random_episode",
    "set_seed",
    "summarize",
]
