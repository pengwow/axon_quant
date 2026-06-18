"""train_ppo.py — PPO 训练完整脚本（stable-baselines3）。

使用 Stable-Baselines3 的 PPO 算法训练交易策略。PPO 适合连续动作
空间（单维目标仓位比例 [-1, 1]），训练稳定、采样高效。

运行前置条件：
    pip install stable-baselines3 gymnasium torch

运行方式：
    cd axon
    /Library/Frameworks/Python.framework/Versions/3.12/bin/python3.12 examples/train_ppo.py \\
        --timesteps 5000 --n-envs 1

设计要点：
- **零外部数据**：使用 `_common.make_synthetic_market_data` 生成 500
  根合成 K 线，避免依赖 parquet / CSV 外部文件。
- **可配置**：通过 CLI 参数调整 timesteps / n_envs / seed / save_path。
- **优雅降级**：若 `stable_baselines3` 不可用，提示用户安装并退出
  退出码 2；不会让脚本"看起来跑了"实际啥也没做。
- **轻量级验证**：默认 `--timesteps 5000`，CI 中 1 分钟内能跑完。
- **可对比**：训练完调用 `model.predict` 跑 1 个 episode，与
  `random_agent.py` 对比 reward 改善。
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


def _try_import_sb3() -> tuple[bool, object, object]:
    try:
        from stable_baselines3 import PPO  # noqa: PLC0415
        from stable_baselines3.common.callbacks import BaseCallback  # noqa: PLC0415

        return True, PPO, BaseCallback
    except ImportError:
        return False, None, None


class ProgressCallback:
    """最小回调：每 `log_every` 步打印一次训练状态。"""

    def __init__(self, log_every: int = 500) -> None:
        self.log_every = log_every
        self.episode_rewards: list[float] = []

    def __call__(self, locals_dict, globals_dict) -> bool:  # noqa: D401
        infos = locals_dict.get("infos", [])
        for info in infos:
            r = info.get("episode")
            if r is not None:
                self.episode_rewards.append(r["r"])
        n_calls = locals_dict.get("self").num_timesteps
        if n_calls % self.log_every == 0 and self.episode_rewards:
            mean_r = sum(self.episode_rewards[-20:]) / min(20, len(self.episode_rewards))
            print(f"  step={n_calls:>6}  ep_rew_mean(20)={mean_r:>10.2f}")
        return True


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="PPO 训练示例")
    p.add_argument("--timesteps", type=int, default=5000, help="总训练步数")
    p.add_argument("--n-envs", type=int, default=1, help="并行环境数")
    p.add_argument("--n-bars", type=int, default=500, help="合成 K 线数量")
    p.add_argument("--learning-rate", type=float, default=3e-4)
    p.add_argument("--batch-size", type=int, default=64)
    p.add_argument("--n-steps", type=int, default=512, help="PPO 每次更新前的采样步数")
    p.add_argument("--gamma", type=float, default=0.99)
    p.add_argument("--seed", type=int, default=42)
    p.add_argument(
        "--save-path",
        type=Path,
        default=None,
        help="模型保存路径；None 时不保存（仅做冒烟测试）",
    )
    p.add_argument(
        "--reward",
        choices=("pnl", "sharpe", "sortino"),
        default="pnl",
    )
    p.add_argument("--log-every", type=int, default=500)
    return p.parse_args()


def main() -> int:
    args = parse_args()
    _common.set_seed(args.seed)

    sb3_ok, PPO, _BaseCallback = _try_import_sb3()
    if not sb3_ok:
        print(
            "ERROR: 需要 `stable-baselines3` 才能跑 PPO 训练。\n"
            "请运行：\n"
            "    pip install stable-baselines3 gymnasium torch\n",
            file=sys.stderr,
        )
        return 2

    # ── 数据 + 环境 ──
    market_data = _common.make_synthetic_market_data(n=args.n_bars, seed=args.seed)
    cfg = _common.make_env_config(max_steps=args.n_bars, seed=args.seed)
    print(
        f"[train_ppo] 准备 {args.n_bars} 根合成 K 线，"
        f"{args.n_envs} 个并行环境，奖励={args.reward}"
    )

    def _env_fn():
        return _vec_env.AxonTradingEnv(
            _common.make_env(config=cfg, market_data=market_data, reward=args.reward)
        )

    venv = _vec_env.make_vec_env(_env_fn, n_envs=args.n_envs, use_stable_baselines3=True)
    print(f"[train_ppo] vec env: {type(venv).__name__}, num_envs={venv.num_envs}")

    # ── 模型 ──
    model = PPO(
        "MlpPolicy",
        venv,
        verbose=0,
        learning_rate=args.learning_rate,
        n_steps=args.n_steps,
        batch_size=args.batch_size,
        gamma=args.gamma,
        seed=args.seed,
    )

    # ── 训练 ──
    print(f"[train_ppo] 开始训练 {args.timesteps} 步 ...")
    t0 = time.perf_counter()
    cb = ProgressCallback(log_every=args.log_every)
    # sb3 的 callback API：传入 BaseCallback 实例或 callable
    try:
        model.learn(total_timesteps=args.timesteps, callback=cb, progress_bar=False)
    except TypeError:
        # 某些 sb3 版本不支持 progress_bar kwarg
        model.learn(total_timesteps=args.timesteps, callback=cb)
    elapsed = time.perf_counter() - t0
    print(f"[train_ppo] 训练完成，耗时 {elapsed:.1f}s")

    # ── 保存（可选）──
    if args.save_path is not None:
        args.save_path.parent.mkdir(parents=True, exist_ok=True)
        model.save(str(args.save_path))
        print(f"[train_ppo] 模型已保存至 {args.save_path}")

    # ── 评估：与随机策略对比 ──
    print("[train_ppo] 评估：1 episode 推理")
    obs = venv.reset()
    total_reward = 0.0
    n_steps = 0
    done = False
    while not done and n_steps < args.n_bars:
        action, _ = model.predict(obs, deterministic=True)
        obs, reward, dones, infos = venv.step(action)
        # VecEnv 环境下 dones 是 array
        done = bool(dones[0]) if hasattr(dones, "__len__") else bool(dones)
        total_reward += float(reward[0]) if hasattr(reward, "__len__") else float(reward)
        n_steps += 1
    print(f"  PPO 评估 episode: {n_steps} 步，累计 reward={total_reward:.2f}")

    # 与随机策略对比
    print("[train_ppo] 对比：1 episode 随机策略")
    env = _env_fn()
    random_res = _common.run_random_episode(env, max_steps=args.n_bars, seed=args.seed)
    print(
        f"  Random episode: {random_res['steps']} 步，累计 reward={random_res['total_reward']:.2f}"
    )

    # ── 验收：模型能跑完不崩溃 ──
    if n_steps < 5:
        print(f"FAIL: PPO 评估仅跑 {n_steps} 步", file=sys.stderr)
        return 1
    print("PASS: PPO 训练流程完成")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
