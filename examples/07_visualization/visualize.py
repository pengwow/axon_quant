"""visualize.py — 回测结果可视化。

生成净值曲线、回撤曲线、交易信号三合图。

运行前置条件：
    pip install matplotlib numpy

运行方式：
    cd axon
    /Library/Frameworks/Python.framework/Versions/3.12/bin/python3.12 examples/visualize.py

设计要点：
- **零外部数据**：使用合成数据运行 PPO 模型，生成可视化结果
- **优雅降级**：若 `matplotlib` 不可用，提示用户安装并退出
- **可配置**：通过 CLI 参数调整模型路径、数据量等
"""

from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

_HERE = Path(__file__).resolve().parent.parent
if str(_HERE) not in sys.path:
    sys.path.insert(0, str(_HERE))

import _common  # noqa: E402
import _vec_env  # noqa: E402


def _try_import_matplotlib():
    """尝试导入 matplotlib，返回 (ok, plt, Figure, axes)。"""
    try:
        import matplotlib.pyplot as plt  # noqa: PLC0415
        from matplotlib.figure import Figure  # noqa: PLC0415

        return True, plt, Figure
    except ImportError:
        return False, None, None


def _try_import_sb3():
    """尝试导入 stable_baselines3。"""
    try:
        from stable_baselines3 import PPO  # noqa: PLC0415

        return True, PPO
    except ImportError:
        return False, None


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="回测结果可视化")
    p.add_argument(
        "--model-path",
        type=Path,
        default=None,
        help="PPO 模型路径；None 时使用随机策略",
    )
    p.add_argument("--n-bars", type=int, default=500, help="合成 K 线数量")
    p.add_argument("--seed", type=int, default=42)
    p.add_argument(
        "--output",
        type=Path,
        default=Path("backtest_results.png"),
        help="输出图片路径",
    )
    p.add_argument("--dpi", type=int, default=150, help="图片 DPI")
    p.add_argument("--show", action="store_true", help="显示图表窗口")
    return p.parse_args()


def run_episode(env, model=None, max_steps=500, seed=42):
    """运行一个 episode，收集可视化数据。"""
    import numpy as np  # noqa: PLC0415

    obs = env.reset()
    if isinstance(obs, tuple):
        obs = obs[0]

    portfolio_values = []
    actions = []
    rewards = []
    done = False
    steps = 0

    while not done and steps < max_steps:
        if model is not None:
            action, _ = model.predict(obs, deterministic=True)
        else:
            # 随机策略
            action = np.array([np.random.uniform(-1, 1)])

        result = env.step(action)
        if len(result) == 5:
            obs, reward, terminated, truncated, info = result
            done = bool(terminated) or bool(truncated)
        else:
            obs, reward, done, info = result

        # 提取净值
        if isinstance(info, dict):
            pv = info.get("portfolio_value", 0.0)
        elif isinstance(info, (list, tuple)) and len(info) > 0:
            pv = info[0].get("portfolio_value", 0.0) if isinstance(info[0], dict) else 0.0
        else:
            pv = 0.0

        portfolio_values.append(float(pv))
        actions.append(float(action[0]) if hasattr(action, "__len__") else float(action))
        rewards.append(float(reward))
        steps += 1

    return {
        "portfolio_values": portfolio_values,
        "actions": actions,
        "rewards": rewards,
        "steps": steps,
    }


def plot_backtest_results(
    data: dict,
    output_path: Path,
    dpi: int = 150,
    show: bool = False,
    title: str = "Backtest Results",
):
    """生成净值曲线 + 回撤曲线 + 交易信号三合图。"""
    import numpy as np  # noqa: PLC0415
    import matplotlib.pyplot as plt  # noqa: PLC0415

    portfolio_values = np.array(data["portfolio_values"])
    actions = np.array(data["actions"])
    steps = np.arange(len(portfolio_values))

    fig, axes = plt.subplots(3, 1, figsize=(14, 10), sharex=True)
    fig.suptitle(title, fontsize=14, fontweight="bold")

    # 1. 净值曲线
    ax1 = axes[0]
    ax1.plot(steps, portfolio_values, label="Portfolio", color="steelblue", linewidth=1.5)
    ax1.set_ylabel("Portfolio Value ($)")
    ax1.legend(loc="upper left")
    ax1.grid(True, alpha=0.3)

    # 2. 回撤曲线
    ax2 = axes[1]
    running_max = np.maximum.accumulate(portfolio_values)
    drawdown = np.where(running_max > 0, (running_max - portfolio_values) / running_max, 0)
    ax2.fill_between(steps, 0, -drawdown * 100, color="salmon", alpha=0.6)
    ax2.set_ylabel("Drawdown (%)")
    ax2.grid(True, alpha=0.3)

    # 3. 交易信号
    ax3 = axes[2]
    ax3.plot(steps, portfolio_values, color="gray", alpha=0.7, linewidth=0.8, label="Portfolio")

    # 买入信号（action > 0.1）
    buy_mask = actions > 0.1
    # 卖出信号（action < -0.1）
    sell_mask = actions < -0.1

    ax3.scatter(
        steps[buy_mask],
        portfolio_values[buy_mask],
        marker="^",
        color="green",
        label="Buy",
        s=20,
        alpha=0.6,
    )
    ax3.scatter(
        steps[sell_mask],
        portfolio_values[sell_mask],
        marker="v",
        color="red",
        label="Sell",
        s=20,
        alpha=0.6,
    )
    ax3.set_ylabel("Portfolio Value ($)")
    ax3.set_xlabel("Step")
    ax3.legend(loc="upper left")
    ax3.grid(True, alpha=0.3)

    plt.tight_layout()
    plt.savefig(str(output_path), dpi=dpi, bbox_inches="tight")
    print(f"[visualize] 图表已保存至 {output_path}")

    if show:
        plt.show()

    plt.close(fig)


def main() -> int:
    args = parse_args()

    # 检查 matplotlib
    mpl_ok, plt, _ = _try_import_matplotlib()
    if not mpl_ok:
        print(
            "ERROR: 需要 `matplotlib` 才能生成可视化图表。\n"
            "请运行：\n"
            "    pip install matplotlib numpy\n",
            file=sys.stderr,
        )
        return 2

    _common.set_seed(args.seed)

    # 准备数据
    market_data = _common.make_synthetic_market_data(n=args.n_bars, seed=args.seed)
    cfg = _common.make_env_config(max_steps=args.n_bars, seed=args.seed)
    env = _common.make_env(config=cfg, market_data=market_data)

    # 加载模型（可选）
    model = None
    if args.model_path is not None:
        sb3_ok, PPO = _try_import_sb3()
        if not sb3_ok:
            print(
                "WARNING: `stable-baselines3` 不可用，使用随机策略。",
                file=sys.stderr,
            )
        else:
            try:
                model = PPO.load(str(args.model_path))
                print(f"[visualize] 已加载模型 {args.model_path}")
            except Exception as e:
                print(f"WARNING: 加载模型失败 ({e})，使用随机策略。", file=sys.stderr)

    # 运行 episode
    print(f"[visualize] 运行 {args.n_bars} 步回测...")
    t0 = time.perf_counter()
    data = run_episode(env, model=model, max_steps=args.n_bars, seed=args.seed)
    elapsed = time.perf_counter() - t0
    print(f"[visualize] 回测完成，耗时 {elapsed:.2f}s，共 {data['steps']} 步")

    # 生成图表
    title = "Backtest Results (PPO)" if model else "Backtest Results (Random)"
    plot_backtest_results(
        data,
        output_path=args.output,
        dpi=args.dpi,
        show=args.show,
        title=title,
    )

    # 打印统计
    import numpy as np  # noqa: PLC0415

    pv = np.array(data["portfolio_values"])
    if len(pv) > 1:
        total_return = (pv[-1] / pv[0] - 1) * 100 if pv[0] > 0 else 0.0
        running_max = np.maximum.accumulate(pv)
        max_dd = np.max((running_max - pv) / running_max) * 100
        print(f"[visualize] 总收益: {total_return:.2f}%")
        print(f"[visualize] 最大回撤: {max_dd:.2f}%")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
