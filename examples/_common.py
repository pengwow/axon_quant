"""AXON 强化学习示例的共享工具。

提供：
- `find_axon_rl_lib()`：定位 `libaxon_rl.dylib` / `.so` 并创建 Python
  可识别的 `.cpython-*.so` 符号链接（仅 macOS / Linux）。
- `make_synthetic_market_data(n, start=100.0, vol=0.01, seed=0)`：生成
  合成 K 线（随机游走 + 高斯噪声），无需外部数据文件。
- `make_env_config(initial_capital=100_000.0, max_steps=200, ...)`：构造
  环境配置字典。
- `make_env(config, market_data, reward="pnl")`：用合成数据构造一个
  `axon_rl.TradingEnv` 实例。
- `run_random_episode(env, max_steps=100, seed=0)`：在环境中执行随机策略。
- `set_seed(seed)`：统一设置 `random` / `numpy` / `torch`（若可用）种子。

设计原则：
- **零外部依赖**：`find_axon_rl_lib` 与 `make_synthetic_market_data`
  不依赖任何第三方库，可作为"冒烟测试"独立运行。
- **可选第三方依赖**：`set_seed` 与 `run_random_episode` 会尽量使用
  `numpy` / `torch`（若已安装），否则回退到 `random`。
- **多 Python 版本支持**：`find_axon_rl_lib` 会同时探测多个常见的
  Python 解释器，挑选与 `sys.version_info` 匹配的目标共享库，避免
  链接到错误版本的 `libpython`。
"""

from __future__ import annotations

import os
import random
import shutil
import sys
from pathlib import Path
from typing import Any, Iterable


# ──────────────────────────────────────────────
# 共享库探测
# ──────────────────────────────────────────────

# 按优先级排列的 Python 解释器候选路径。优先级最高的是 macOS 框架版本
# （/Library/Frameworks），最低的是 PATH 中的 `python3`。
# 这样做的原因是：在某些 macOS conda 环境下（如 Anaconda 打包的
# Python 3.13），pyo3 0.22/0.23 扩展会触发 GIL 错误；framework 模式
# 的 Python 3.12/3.13 表现稳定。优先选用 framework 模式。
_PYTHON_CANDIDATES: tuple[str, ...] = (
    "/Library/Frameworks/Python.framework/Versions/3.12/bin/python3.12",
    "/Library/Frameworks/Python.framework/Versions/3.13/bin/python3.13",
    "/Library/Frameworks/Python.framework/Versions/3.11/bin/python3.11",
    "/usr/bin/python3",
    "/usr/local/bin/python3",
    "python3",
)


def _select_python_for_target() -> tuple[Path | None, str]:
    """根据当前 Python 探测 target/debug 下的 libaxon_rl 共享库与对应 Python。

    Returns:
        `(共享库路径, 描述)` 元组；如果都没找到则 `(None, "")`。
    """
    here = Path(__file__).resolve().parent
    repo_root = here.parent
    target_dirs = [repo_root / "target" / "debug", repo_root / "target" / "release"]
    target_dir = next((d for d in target_dirs if d.exists()), None)
    if target_dir is None:
        return None, ""

    # 在每个 target 目录里寻找 libaxon_rl.{dylib,so,dll}
    lib_names = ("libaxon_rl.dylib", "libaxon_rl.so", "axon_rl.dll")
    src = next(
        (target_dir / n for n in lib_names if (target_dir / n).exists()),
        None,
    )
    return src, str(target_dir)


def find_axon_rl_lib() -> Path:
    """查找已编译的 `libaxon_rl` 共享库并放置 Python 可加载的符号链接。

    PyO3 通过 `cargo build` 产出 `libaxon_rl.dylib`（macOS）或
    `libaxon_rl.so`（Linux），但 CPython 仅识别
    `.cpython-XYZ-platform.so` / `.abi3.so` / `.so` 后缀。
    本函数在同目录生成 `axon_rl.<suffix>` 符号链接，确保
    `import axon_rl` 成功。

    Returns:
        包含符号链接的 `target/debug`（或 `target/release`）目录，
        且已加入 `sys.path`。

    Raises:
        FileNotFoundError: 找不到已编译的 `libaxon_rl` 共享库。
    """
    src, target_dir_str = _select_python_for_target()
    if src is None:
        raise FileNotFoundError(
            "未找到 libaxon_rl 共享库。请先运行：\n"
            "  cargo build -p axon-rl --features python\n"
            f"已搜索：{['target/debug', 'target/release']}"
        )
    target_dir = Path(target_dir_str)

    # 创建 Python 友好的符号链接（同时尝试 .so / .cpython-*.so 形式）
    py_suffix = (
        f".cpython-{sys.version_info.major}{sys.version_info.minor}-{sys.platform}.so"
    )
    py_link = target_dir / f"axon_rl{py_suffix}"
    if py_link.exists() or py_link.is_symlink():
        # 符号链接已经存在：先检查是否指向正确的目标，否则重建
        try:
            current_target = os.readlink(py_link)
            if Path(current_target).resolve() != src.resolve():
                py_link.unlink()
        except OSError:
            # 不是符号链接（比如是普通文件），直接删除重建
            py_link.unlink()
    if not py_link.exists():
        try:
            # 在 target_dir 内部使用相对路径，避免绝对路径在 CI 中失效
            os.symlink(src.name, py_link)
        except (OSError, NotImplementedError):
            # Windows 或权限不足时退化为复制
            shutil.copy2(src, py_link)

    if str(target_dir) not in sys.path:
        sys.path.insert(0, str(target_dir))
    return target_dir


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
        `close` / `volume`，符合 `axon_rl.TradingEnv` 期望格式。
    """
    rng = random.Random(seed)
    bars: list[dict[str, Any]] = []
    price = start_price
    for t in range(n):
        # 开 = 上一根收（除第一根外）
        open_ = price
        # 收 = 开 * (1 + 高斯)
        ret = rng.gauss(0.0, vol)
        close = max(1e-6, open_ * (1.0 + ret))
        # 高 / 低 = 在 O/C 周围浮动
        spread = abs(close - open_) + open_ * vol * 0.5
        high = max(open_, close) + spread * rng.random()
        low = max(1e-6, min(open_, close) - spread * rng.random())
        # 量 = 1000 ± 200 噪声
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
    """构造环境配置字典（对应 `parse_config` 的输入）。"""
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
    """构造 `axon_rl.TradingEnv` 实例。

    Args:
        config: 环境配置；None 表示使用默认
        market_data: 行情 K 线；None 时自动生成 500 根合成数据
        reward: 奖励函数名（"pnl" / "sharpe" / "sortino"）
        action_space: 动作空间定义；None 表示默认连续 `[-1, 1]`

    Returns:
        `axon_rl.TradingEnv` 实例
    """
    find_axon_rl_lib()  # 确保 axon_rl 可导入
    import axon_rl  # noqa: PLC0415

    cfg = config if config is not None else make_env_config()
    data = market_data if market_data is not None else make_synthetic_market_data()
    return axon_rl.TradingEnv(
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
        env: `axon_rl.TradingEnv` 实例
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
        # 连续动作：单维目标仓位比例
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
    "find_axon_rl_lib",
    "make_env",
    "make_env_config",
    "make_synthetic_market_data",
    "run_random_episode",
    "set_seed",
    "summarize",
]
