"""train_sac.py — SAC 训练完整脚本（stable-baselines3）。

SAC（Soft Actor-Critic）适用于连续动作空间，可以输出仓位比例
（[-1, 1]），适合精细化仓位管理。

运行前置条件：
    pip install stable-baselines3 gymnasium torch

运行方式：
    cd axon
    /Library/Frameworks/Python.framework/Versions/3.12/bin/python3.12 examples/train_sac.py \\
        --timesteps 5000 --n-envs 1

与 `train_ppo.py` 的差异：
- 算法：SAC（off-policy，最大熵） vs PPO（on-policy）
- 适用场景：SAC 适合样本效率要求高、可重复利用 replay buffer 的场景；
  PPO 适合训练稳定性优先、不方便开 replay buffer 的场景。
- 训练时间：SAC 收敛通常需要的步数更少，但每次 step 更重（本环境
  step 极快，所以差异不大）。
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


def _try_import_sb3() -> tuple[bool, object]:
    try:
        from stable_baselines3 import SAC  # noqa: PLC0415

        return True, SAC
    except ImportError:
        return False, None


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="SAC 训练示例")
    p.add_argument("--timesteps", type=int, default=5000)
    p.add_argument("--n-envs", type=int, default=1)
    p.add_argument("--n-bars", type=int, default=500)
    p.add_argument("--learning-rate", type=float, default=3e-4)
    p.add_argument("--buffer-size", type=int, default=10_000)
    p.add_argument("--batch-size", type=int, default=256)
    p.add_argument("--gamma", type=float, default=0.99)
    p.add_argument("--tau", type=float, default=0.005)
    p.add_argument("--seed", type=int, default=42)
    p.add_argument(
        "--save-path",
        type=Path,
        default=None,
        help="模型保存路径；None 时不保存",
    )
    p.add_argument(
        "--reward",
        choices=("pnl", "sharpe", "sortino"),
        default="sharpe",
        help="SAC 推荐用风险调整奖励（sharpe/sortino）减少方差",
    )
    return p.parse_args()


def main() -> int:
    args = parse_args()
    _common.set_seed(args.seed)

    sb3_ok, SAC = _try_import_sb3()
    if not sb3_ok:
        print(
            "ERROR: 需要 `stable-baselines3` 才能跑 SAC 训练。\n"
            "请运行：\n"
            "    pip install stable-baselines3 gymnasium torch\n",
            file=sys.stderr,
        )
        return 2

    # ── 数据 + 环境 ──
    market_data = _common.make_synthetic_market_data(n=args.n_bars, seed=args.seed)
    cfg = _common.make_env_config(max_steps=args.n_bars, seed=args.seed)
    print(
        f"[train_sac] 准备 {args.n_bars} 根合成 K 线，"
        f"{args.n_envs} 个并行环境，奖励={args.reward}"
    )

    def _env_fn():
        return _vec_env.AxonTradingEnv(
            _common.make_env(config=cfg, market_data=market_data, reward=args.reward)
        )

    venv = _vec_env.make_vec_env(_env_fn, n_envs=args.n_envs, use_stable_baselines3=True)
    print(f"[train_sac] vec env: {type(venv).__name__}, num_envs={venv.num_envs}")

    # ── 模型 ──
    model = SAC(
        "MlpPolicy",
        venv,
        verbose=0,
        learning_rate=args.learning_rate,
        buffer_size=args.buffer_size,
        batch_size=args.batch_size,
        gamma=args.gamma,
        tau=args.tau,
        seed=args.seed,
    )

    # ── 训练 ──
    print(f"[train_sac] 开始训练 {args.timesteps} 步 ...")
    t0 = time.perf_counter()
    try:
        model.learn(total_timesteps=args.timesteps, progress_bar=False)
    except TypeError:
        model.learn(total_timesteps=args.timesteps)
    elapsed = time.perf_counter() - t0
    print(f"[train_sac] 训练完成，耗时 {elapsed:.1f}s")

    # ── 保存（可选）──
    if args.save_path is not None:
        args.save_path.parent.mkdir(parents=True, exist_ok=True)
        model.save(str(args.save_path))
        print(f"[train_sac] 模型已保存至 {args.save_path}")

    # ── 评估：1 episode 推理 ──
    print("[train_sac] 评估：1 episode 推理")
    obs = venv.reset()
    total_reward = 0.0
    n_steps = 0
    done = False
    while not done and n_steps < args.n_bars:
        action, _ = model.predict(obs, deterministic=True)
        obs, reward, dones, _infos = venv.step(action)
        done = bool(dones[0]) if hasattr(dones, "__len__") else bool(dones)
        total_reward += float(reward[0]) if hasattr(reward, "__len__") else float(reward)
        n_steps += 1
    print(f"  SAC 评估 episode: {n_steps} 步，累计 reward={total_reward:.2f}")

    # ── 对比：随机策略 ──
    print("[train_sac] 对比：1 episode 随机策略")
    env = _env_fn()
    random_res = _common.run_random_episode(env, max_steps=args.n_bars, seed=args.seed)
    print(
        f"  Random episode: {random_res['steps']} 步，累计 reward={random_res['total_reward']:.2f}"
    )

    # ── 验收 ──
    if n_steps < 5:
        print(f"FAIL: SAC 评估仅跑 {n_steps} 步", file=sys.stderr)
        return 1
    print("PASS: SAC 训练流程完成")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
