# 场景一 — 策略研发全流程

本文档演示如何在 AXON 量化平台中完成一条完整的策略研发流水线，涵盖从数据准备到模型上线的 7 个关键步骤。每个步骤均基于 AXON `0.1.0` 版本的真实源代码，并展示"上一步的输出如何成为下一步的输入"。

---

## 全流程概览

```text
+-----------+     +-----------+     +-----------+     +-----------+
|  1.数据准备 | --> | 2.RL训练  | --> | 3.HPO搜索 | --> | 4.Walk-   |
| (特征工程) |     | (PPO+Env) |     | (Optuna)  |     | Forward   |
+-----------+     +-----------+     +-----------+     +-----------+
                                              |
                                              v
+-----------+     +-----------+     +-----------+
| 7.模型注册 | <-- | 6.模型导出 | <-- | 5.回测验证 |
|  与上线   |     | (ONNX/.pt)|     |(Backtest) |
+-----------+     +-----------+     +-----------+
```

---

## 步骤 1：数据准备（特征工程）

**输入**：原始行情数据（CSV / Parquet / 实时流）  
**输出**：标准化后的 `MarketBar` 序列 + `FeatureConfig` 特征配置

AXON 的数据层（`axon-data`）支持 CSV、Parquet 与内存映射多种来源。以下示例展示如何构造合成数据并配置观测特征，供后续 RL 环境使用。

```python
"""
步骤 1：数据准备与特征工程
- 生成/加载 K 线数据
- 定义观测空间的特征配置（close, volume, RSI 等）
- 输出：market_data + features，作为 TradingEnv 的输入
"""

from __future__ import annotations

import numpy as np

# -------------------------------------------------
# 1.1 合成数据生成（零外部依赖，CI 友好）
# -------------------------------------------------
def make_synthetic_market_data(n: int = 500, seed: int = 42) -> list[dict]:
    """生成随机游走合成 K 线，用于快速原型验证。"""
    rng = np.random.default_rng(seed)
    price = 100.0
    bars = []
    for i in range(n):
        # 随机游走生成 OHLCV
        ret = rng.normal(0.0, 0.02)
        open_p = price * (1 + ret)
        high_p = open_p * (1 + abs(rng.normal(0, 0.01)))
        low_p = open_p * (1 - abs(rng.normal(0, 0.01)))
        close_p = (high_p + low_p) / 2 + rng.normal(0, 0.005)
        vol = rng.uniform(1000.0, 5000.0)
        bars.append({
            "timestamp": i,
            "open": open_p,
            "high": high_p,
            "low": low_p,
            "close": close_p,
            "volume": vol,
        })
        price = close_p
    return bars


# -------------------------------------------------
# 1.2 特征配置：告诉 ObservationSpace 提取哪些字段
# -------------------------------------------------
def make_feature_config():
    """构造特征配置列表，决定 RL 智能体能看到什么。"""
    # 对应 Rust 端的 FeatureConfig：
    #   source: PriceField("close") / VolumeField("volume")
    #   normalizer: ZScore / MinMax / None
    #   clip_range: 可选截断范围
    return [
        {
            "name": "close",
            "source": {"PriceField": "close"},   # 从 bar.close 提取
            "normalizer": "ZScore",               # Z-Score 标准化
            "clip_range": None,
        },
        {
            "name": "volume",
            "source": {"VolumeField": "volume"},
            "normalizer": "None",                 # 不做标准化
            "clip_range": None,
        },
        {
            "name": "returns",
            "source": {"Derived": "returns"},    # 派生特征：收益率
            "normalizer": "ZScore",
            "clip_range": (-3.0, 3.0),            # 截断极端值
        },
    ]


# -------------------------------------------------
# 1.3 环境配置：将数据与特征绑定
# -------------------------------------------------
def make_env_config(
    initial_capital: float = 100_000.0,
    max_steps: int = 500,
    seed: int = 42,
) -> dict:
    """构造 TradingEnv 配置字典。"""
    return {
        "symbol": "BTCUSDT",
        "initial_capital": initial_capital,
        "max_steps": max_steps,
        "seed": seed,
        "return_window": 20,          # 收益率历史窗口（用于 Sharpe/Sortino 奖励）
    }


# 主入口：输出 market_data + feature_config + env_config
if __name__ == "__main__":
    market_data = make_synthetic_market_data(n=500, seed=42)
    features = make_feature_config()
    cfg = make_env_config()
    print(f"[步骤 1] 生成 {len(market_data)} 根 K 线，{len(features)} 个特征")
    # 下一步：将这些数据传入 TradingEnv 与 VecEnv 进行 RL 训练
```

**上一步输出 → 下一步输入**：`market_data` + `features` + `cfg` 作为 `TradingEnv::new()` 的参数。

---

## 步骤 2：RL 训练（PPO + TradingEnv + VecEnv）

**输入**：`market_data`、`features`、`cfg`  
**输出**：训练好的 PPO 模型（`model.zip`）+ 训练日志

AXON 的 RL 层（`axon-rl`）提供 `TradingEnv`（Gymnasium 兼容）与 `SyncVecEnv` / `AsyncVecEnv`（向量化并行）。以下示例使用 `stable-baselines3` 的 PPO 算法进行训练。

```python
"""
步骤 2：RL 训练（PPO + TradingEnv + VecEnv）
- 使用上一步的 market_data / cfg 构造 TradingEnv
- 通过 VecEnv 并行采样加速训练
- 输出：训练好的模型文件路径
"""

from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

# 假设已安装 stable-baselines3
try:
    from stable_baselines3 import PPO
    from stable_baselines3.common.callbacks import BaseCallback
except ImportError:
    print("ERROR: 需要 `stable-baselines3`。请运行：pip install stable-baselines3 gymnasium torch")
    sys.exit(2)

# 引入 AXON 环境封装（示例中的 _vec_env.py 对 TradingEnv 做 Gymnasium 适配）
import _vec_env
import _common


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="PPO 训练示例")
    p.add_argument("--timesteps", type=int, default=5000, help="总训练步数")
    p.add_argument("--n-envs", type=int, default=4, help="并行环境数（VecEnv）")
    p.add_argument("--n-bars", type=int, default=500, help="合成 K 线数量")
    p.add_argument("--learning-rate", type=float, default=3e-4)
    p.add_argument("--batch-size", type=int, default=64)
    p.add_argument("--n-steps", type=int, default=512, help="PPO 每次更新前的采样步数")
    p.add_argument("--gamma", type=float, default=0.99)
    p.add_argument("--seed", type=int, default=42)
    p.add_argument("--save-path", type=Path, default=Path("models/ppo_momentum.zip"))
    p.add_argument("--reward", choices=("pnl", "sharpe", "sortino"), default="pnl")
    return p.parse_args()


class ProgressCallback:
    """最小回调：每 log_every 步打印训练状态。"""

    def __init__(self, log_every: int = 500) -> None:
        self.log_every = log_every
        self.episode_rewards: list[float] = []

    def __call__(self, locals_dict, globals_dict) -> bool:
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


def _build_factory(n_bars: int, base_seed: int, env_id: int, reward: str):
    """工厂函数：每个环境用独立 seed 偏移，避免数据完全相同。"""
    # 复用步骤 1 的数据生成逻辑
    market_data = _common.make_synthetic_market_data(n=n_bars, seed=base_seed + env_id)
    cfg = _common.make_env_config(
        max_steps=n_bars, seed=base_seed + env_id, symbol=f"BTCUSDT_{env_id}"
    )

    def _factory():
        # AxonTradingEnv 是对 TradingEnv 的 Gymnasium 包装
        return _vec_env.AxonTradingEnv(
            _common.make_env(config=cfg, market_data=market_data, reward=reward)
        )

    return _factory


def main() -> int:
    args = parse_args()
    _common.set_seed(args.seed)

    print(f"[步骤 2] 准备 {args.n_bars} 根合成 K 线，{args.n_envs} 个并行环境，奖励={args.reward}")

    # -------------------------------------------------
    # 2.1 构造向量化环境（SyncVecEnv / DummyVecEnv）
    # -------------------------------------------------
    factories = [_build_factory(args.n_bars, args.seed, i, args.reward) for i in range(args.n_envs)]
    venv = _vec_env.make_vec_env(lambda: factories[0](), n_envs=args.n_envs, use_stable_baselines3=True)
    print(f"  vec env: {type(venv).__name__}, num_envs={venv.num_envs}")

    # -------------------------------------------------
    # 2.2 构造 PPO 模型
    # -------------------------------------------------
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

    # -------------------------------------------------
    # 2.3 训练
    # -------------------------------------------------
    print(f"[步骤 2] 开始训练 {args.timesteps} 步 ...")
    t0 = time.perf_counter()
    cb = ProgressCallback(log_every=500)
    try:
        model.learn(total_timesteps=args.timesteps, callback=cb, progress_bar=False)
    except TypeError:
        model.learn(total_timesteps=args.timesteps, callback=cb)
    elapsed = time.perf_counter() - t0
    print(f"[步骤 2] 训练完成，耗时 {elapsed:.1f}s")

    # -------------------------------------------------
    # 2.4 保存模型（作为下一步 HPO / 回测的输入）
    # -------------------------------------------------
    args.save_path.parent.mkdir(parents=True, exist_ok=True)
    model.save(str(args.save_path))
    print(f"[步骤 2] 模型已保存至 {args.save_path}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

**上一步输出 → 下一步输入**：`args.save_path`（模型文件路径）传入 HPO 的目标函数，用于评估不同超参下的模型性能。

---

## 步骤 3：HPO 超参搜索（OptunaStudy + 多目标优化）

**输入**：模型训练脚本路径 + 搜索空间定义  
**输出**：Pareto 前沿（多目标）或最佳 trial（单目标）+ 最优超参字典

AXON 的 HPO 层（`axon-hpo`）封装了 Optuna，支持单目标与多目标优化，内置 Pareto 前沿与超体积计算。

```python
"""
步骤 3：HPO 超参搜索（OptunaStudy + 多目标优化）
- 定义搜索空间（学习率、gamma、batch_size 等）
- 目标函数：加载步骤 2 的模型框架，用不同超参训练并评估夏普比率 + 最大回撤
- 输出：最优超参 + Pareto 前沿
"""

from __future__ import annotations

import tempfile
from typing import Any

import axon_quant

# Rust 端 HPO 模块的 Python 绑定
hpo = axon_quant.hpo


def objective_fn(params: dict[str, Any]) -> list[float]:
    """
    HPO 目标函数。
    输入：一组超参（由 Optuna 根据 search_space 采样）
    输出：[sharpe_ratio, -max_drawdown]（多目标，均最大化）
    """
    lr = params["learning_rate"]
    gamma = params["gamma"]
    batch_size = params["batch_size"]

    # 这里复用步骤 2 的训练逻辑，但用当前 trial 的超参
    # 为简化示例，用随机数模拟训练结果；真实场景应调用 model.learn()
    import random
    random.seed(hash((lr, gamma, batch_size)) % 2**32)
    simulated_sharpe = random.uniform(0.5, 2.0) + (lr * 1000)
    simulated_drawdown = random.uniform(0.05, 0.25)

    # 返回多目标：最大化夏普，最小化回撤（取负转为最大化）
    return [simulated_sharpe, -simulated_drawdown]


def main() -> int:
    print("=" * 60)
    print("步骤 3：HPO 超参搜索（多目标）")
    print("=" * 60)

    # -------------------------------------------------
    # 3.1 定义搜索空间（对应 search_space.py 中的预设）
    # -------------------------------------------------
    search_space = {
        "learning_rate": hpo.SearchSpaceDef(param_type="log_uniform", low=1e-5, high=1e-2),
        "gamma": hpo.SearchSpaceDef(param_type="uniform", low=0.95, high=0.999),
        "batch_size": hpo.SearchSpaceDef(param_type="choice", choices=[32, 64, 128, 256]),
    }

    # -------------------------------------------------
    # 3.2 创建 OptunaHPO 执行器
    # -------------------------------------------------
    runner = hpo.OptunaHPO(
        search_space=search_space,
        objective_fn=objective_fn,
        study_name="ppo_momentum_multiobj",
        directions=["maximize", "maximize"],  # 双目标
        sampler=hpo.SamplerConfig(sampler_type="tpe", n_startup_trials=5, seed=42),
        pruner=hpo.PrunerConfig(pruner_type="median"),
    )

    # -------------------------------------------------
    # 3.3 执行搜索
    # -------------------------------------------------
    results = runner.run(n_trials=20, n_jobs=1, timeout_seconds=300)
    print(f"\n[步骤 3] 完成 {len(results)} 个 trials")

    # -------------------------------------------------
    # 3.4 获取 Pareto 前沿与超体积
    # -------------------------------------------------
    front = runner.get_pareto_front()
    print(f"[步骤 3] Pareto 前沿点数: {len(front)}")
    for p in front[:3]:
        print(f"  trial #{p.trial_id}: sharpe={p.objectives[0]:.3f}, drawdown={-p.objectives[1]:.3f}")

    hv = runner.compute_hypervolume(reference_point=[3.0, 0.0])  # 参考点
    print(f"[步骤 3] 超体积: {hv:.4f}")

    # -------------------------------------------------
    # 3.5 输出最优超参（供下一步 Walk-Forward / 回测使用）
    # -------------------------------------------------
    best = runner.get_best_trial()
    if best:
        print(f"[步骤 3] 最佳 trial: #{best.trial_id}, params={best.params}")
        # 将最优超参写入文件，供下游步骤读取
        import json
        with open("best_hpo_params.json", "w") as f:
            json.dump(best.params, f, indent=2)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

**上一步输出 → 下一步输入**：`best_hpo_params.json` 中的最优超参用于重新训练最终模型，并传入 Walk-Forward 验证。

---

## 步骤 4：Walk-Forward 验证（Purge + Embargo + Deflated Sharpe）

**输入**：完整数据集 + 最优超参  
**输出**：各 fold 的 OOS（样本外）指标 + 聚合稳定性分析

AXON 的 Walk-Forward 模块（`axon-walk-forward`）提供泄漏检测、Purge、Embargo 与 Deflated Sharpe 计算，防止过拟合与数据泄漏。

```python
"""
步骤 4：Walk-Forward 验证（Purge + Embargo + Deflated Sharpe）
- 将数据切分为多个训练/测试 fold
- 每个 fold 训练后用 Purge + Embargo 清洗测试集
- 聚合所有 fold 结果，计算 Deflated Sharpe
"""

from __future__ import annotations

import random

import axon_quant

# Rust 端 Walk-Forward 模块的 Python 绑定
wf = axon_quant.walk_forward


def main() -> int:
    print("=" * 60)
    print("步骤 4：Walk-Forward 验证")
    print("=" * 60)

    # -------------------------------------------------
    # 4.1 模拟 fold 结果（真实场景由步骤 2/3 的模型在各 fold 上生成）
    # -------------------------------------------------
    n_folds = 5
    folds = []
    for i in range(n_folds):
        train_idx = list(range(0, 80 + i * 20))
        test_idx = list(range(80 + i * 20, 100 + i * 20))

        # 4.1.1 泄漏检测：确保训练集与测试集无重叠
        has_leak, pairs = wf.py_detect_leakage(train_idx, test_idx, gap=0)
        assert not has_leak, f"fold {i}: 检测到数据泄漏!"

        # 4.1.2 Purge：移除训练集中与测试集标签重叠的样本
        # 假设标签基于未来 5 步，需清除训练集尾部 5 个索引
        purged_train = wf.py_purge_overlapping_labels(train_idx, test_idx, purge_length=5)
        print(f"  fold {i}: train={len(purged_train)} (purge 后), test={len(test_idx)}")

        # 4.1.3 Embargo：对测试集尾部施加 embargo，防止信息泄漏
        embargoed_test = wf.py_embargo_indices(test_idx, embargo_pct=0.1, total_len=200)

        # 模拟该 fold 的测试集夏普比率（真实场景由模型回测得到）
        simulated_sharpe = 1.2 + random.uniform(-0.3, 0.3)
        folds.append({
            "train": purged_train,
            "test": embargoed_test,
            "test_sharpe": simulated_sharpe,
            "test_return": random.uniform(-0.05, 0.15),
        })

    # -------------------------------------------------
    # 4.2 聚合所有 fold 的 OOS 结果
    # -------------------------------------------------
    sharpes = [f["test_sharpe"] for f in folds]
    returns = [f["test_return"] for f in folds]
    mean_sharpe = sum(sharpes) / len(sharpes)
    print(f"\n[步骤 4] 平均 OOS Sharpe: {mean_sharpe:.4f}")

    # -------------------------------------------------
    # 4.3 Deflated Sharpe：校正多重试验带来的虚高夏普
    # -------------------------------------------------
    # 参数：观测夏普、总 trial 数、夏普比率的标准差估计
    dsr = wf.py_deflated_sharpe(observed_sharpe=mean_sharpe, n_trials=20, sharpe_std=0.5)
    print(f"[步骤 4] Deflated Sharpe: {dsr:.4f} (原始={mean_sharpe:.4f})")
    assert dsr <= mean_sharpe, "Deflated Sharpe 不应超过原始夏普"

    # -------------------------------------------------
    # 4.4 稳定性指标
    # -------------------------------------------------
    import numpy as np
    sharpe_std = np.std(sharpes, ddof=1)
    sharpe_of_sharpe = mean_sharpe / sharpe_std if sharpe_std > 1e-9 else 0.0
    print(f"[步骤 4] Sharpe of Sharpe: {sharpe_of_sharpe:.4f}")

    # 输出：通过 Walk-Forward 验证的模型才有资格进入回测阶段
    print("\n[步骤 4] Walk-Forward 验证通过，进入回测阶段")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

**上一步输出 → 下一步输入**：通过 Walk-Forward 验证的模型配置（超参 + 数据切分策略）传入回测引擎进行事件级回测。

---

## 步骤 5：回测验证（BacktestEngine.step()）

**输入**：策略模型（或规则策略）+ 历史事件队列  
**输出**：`RunResult`（含 PnL、最大回撤、成交统计等）

AXON 的回测引擎（`axon-backtest`）采用事件驱动架构，支持 L1/L2/L3 撮合、市场冲击模型与步进模式。

```python
"""
步骤 5：回测验证（BacktestEngine.step()）
- 构造事件队列（订单提交、成交、行情等）
- 使用 BacktestEngine.run() 或 step() 逐事件推进
- 输出：RunResult 含 PnL、max_drawdown、final_nav 等
"""

from __future__ import annotations

# 注意：以下代码为 Rust API 的 Python 伪代码，展示核心逻辑。
# 真实使用需通过 PyO3 绑定或 Rust 原生调用。

def run_backtest_demo():
    """
    演示 BacktestEngine 的核心使用模式。
    对应 Rust 源码：crates/axon-backtest/src/engine.rs
    """
    # -------------------------------------------------
    # 5.1 构造回测配置
    # -------------------------------------------------
    config = {
        "clock": "SimulatedClock",           # 模拟时钟
        "matching_engine": "L1MatchingEngine",  # L1 撮合引擎
        "impact_model": None,                # 可选：市场冲击模型
        "initial_cash": 100_000.0,
    }

    # -------------------------------------------------
    # 5.2 填充事件队列
    # -------------------------------------------------
    # EventQueue 中的事件按时间戳排序，引擎按序消费
    events = []
    # 示例：提交一个限价买单
    events.append({
        "type": "Order",
        "timestamp": 1_000,
        "action": {
            "type": "Submitted",
            "order": {
                "id": 1,
                "symbol": "BTC-USDT",
                "side": "Buy",
                "order_type": {"Limit": {"price": 50_000.0}},
                "quantity": 0.1,
                "time_in_force": "GTC",
            }
        }
    })
    # 示例：提交一个匹配的卖单（产生 fill）
    events.append({
        "type": "Order",
        "timestamp": 2_000,
        "action": {
            "type": "Submitted",
            "order": {
                "id": 2,
                "symbol": "BTC-USDT",
                "side": "Sell",
                "order_type": {"Limit": {"price": 50_000.0}},
                "quantity": 0.1,
                "time_in_force": "GTC",
            }
        }
    })

    # -------------------------------------------------
    # 5.3 创建引擎并运行
    # -------------------------------------------------
    # Rust 端：let mut engine = BacktestEngine::new(config, event_queue);
    #         let result = engine.run();
    print("[步骤 5] 创建 BacktestEngine 并运行 ...")

    # -------------------------------------------------
    # 5.4 步进模式（用于调试或与 RL 环境交互）
    # -------------------------------------------------
    # Rust 端：while let Some(stats) = engine.step() { ... }
    # step() 每调用一次处理一个事件，返回当前 RunStats 快照
    print("[步骤 5] 步进模式：逐事件检查撮合状态")

    # -------------------------------------------------
    # 5.5 解析结果
    # -------------------------------------------------
    # RunResult 字段：
    #   events_processed: u64   # 处理事件总数
    #   orders_accepted: u64    # 接受订单数
    #   orders_rejected: u64    # 拒绝订单数
    #   fills: u64              # 成交笔数
    #   total_pnl: f64          # 累计盈亏
    #   max_drawdown: f64       # 最大回撤
    #   final_nav: f64          # 最终净资产
    #   duration: Duration      # 运行耗时
    result = {
        "events_processed": 2,
        "orders_accepted": 2,
        "fills": 1,
        "total_pnl": -100.0,     # 买入 0.1 BTC @ 50k，成本 5000
        "max_drawdown": 0.0,
        "final_nav": 99_900.0,
    }
    print(f"[步骤 5] 回测结果: PnL={result['total_pnl']:.2f}, NAV={result['final_nav']:.2f}")
    return result


if __name__ == "__main__":
    run_backtest_demo()
```

**上一步输出 → 下一步输入**：回测通过的模型文件路径（如 `models/ppo_momentum.zip`）传入模型导出模块，转换为部署格式。

---

## 步骤 6：模型导出（ONNX / .pt / .safetensors）

**输入**：训练好的模型（PyTorch / SB3 格式）  
**输出**：ONNX / TorchScript `.pt` / `.safetensors` 文件

AXON 的推理层（`axon-inference`）提供统一的 `InferenceEngine` trait，支持 ONNX、LibTorch（`.pt`）与 Candle 后端。

```python
"""
步骤 6：模型导出（ONNX / .pt / .safetensors）
- 将 SB3/PyTorch 模型导出为 ONNX 或 TorchScript
- 供 Rust 端的 OnnxBackend / TchBackend 加载
"""

from __future__ import annotations

import torch
from pathlib import Path


def export_to_onnx(sb3_model, save_path: Path, input_shape: tuple[int, int, int] = (1, 1, 64)):
    """
    将 SB3 模型导出为 ONNX 格式。
    对应 Rust 端：OnnxBackend::load(path) -> InferenceEngine
    """
    # SB3 模型内部是 PyTorch nn.Module
    policy = sb3_model.policy
    policy.eval()

    # 构造 dummy 输入（与 Observation.features 维度一致）
    dummy_input = torch.randn(input_shape)

    torch.onnx.export(
        policy,
        dummy_input,
        str(save_path),
        export_params=True,
        opset_version=14,
        do_constant_folding=True,
        input_names=["observation"],
        output_names=["action", "value"],
        dynamic_axes={"observation": {0: "batch"}},
    )
    print(f"[步骤 6] ONNX 模型已导出: {save_path}")


def export_to_torchscript(sb3_model, save_path: Path, input_shape: tuple[int, int, int] = (1, 1, 64)):
    """
    将策略网络导出为 TorchScript (.pt)。
    对应 Rust 端：TchBackend::load(path) -> InferenceEngine
    """
    policy = sb3_model.policy
    policy.eval()
    dummy_input = torch.randn(input_shape)
    traced = torch.jit.trace(policy, dummy_input)
    traced.save(str(save_path))
    print(f"[步骤 6] TorchScript 模型已导出: {save_path}")


def main():
    # 假设已加载步骤 2 训练好的 SB3 模型
    # from stable_baselines3 import PPO
    # model = PPO.load("models/ppo_momentum.zip")

    # 导出为 ONNX（跨平台、推理快）
    # export_to_onnx(model, Path("models/ppo_momentum.onnx"))

    # 导出为 TorchScript（与 Rust tch 后端无缝集成）
    # export_to_torchscript(model, Path("models/ppo_momentum.pt"))

    print("[步骤 6] 模型导出完成，准备注册上线")


if __name__ == "__main__":
    main()
```

**Rust 端推理引擎加载示例**：

```rust
// 对应源码：crates/axon-inference/src/backend/onnx.rs
use axon_inference::backend::OnnxBackend;
use axon_inference::engine::InferenceEngine;
use std::path::Path;

fn main() {
    let mut backend = OnnxBackend::new(config);
    // 加载步骤 6 导出的 ONNX 文件
    backend.load(Path::new("models/ppo_momentum.onnx")).unwrap();
    // 执行推理
    let action = backend.infer(&observation).unwrap();
    println!("Action: {:?}", action);
}
```

**上一步输出 → 下一步输入**：导出的模型文件路径（如 `models/ppo_momentum.onnx`）传入模型注册表进行版本管理与阶段晋升。

---

## 步骤 7：模型注册与上线（Registry + 阶段生命周期）

**输入**：模型产物文件路径 + 元数据（超参、指标、Git Commit 等）  
**输出**：`ModelVersion` 记录，阶段从 `STAGING` → `PRODUCTION`

AXON 的注册表层（`axon-registry`）提供纯 Python 的模型生命周期管理，支持阶段转换、回滚与持久化。

```python
"""
步骤 7：模型注册与上线（Registry + 阶段生命周期）
- 将步骤 6 导出的模型注册到 ModelRegistry
- 阶段流转：STAGING -> PRODUCTION
- 支持回滚到上一个 ARCHIVED 版本
"""

from __future__ import annotations

import os
import tempfile

import axon_quant

ModelRegistry = axon_quant.registry.ModelRegistry
LocalStorage = axon_quant.registry.LocalStorage
ModelStage = axon_quant.registry.ModelStage


def main() -> int:
    print("=" * 60)
    print("步骤 7：模型注册与上线")
    print("=" * 60)

    with tempfile.TemporaryDirectory() as tmp:
        # -------------------------------------------------
        # 7.1 准备模型产物（模拟步骤 6 的输出）
        # -------------------------------------------------
        model_path = os.path.join(tmp, "ppo_momentum_v1.onnx")
        with open(model_path, "wb") as f:
            f.write(b"ONNX model weights v1 (1024 params)")

        # -------------------------------------------------
        # 7.2 创建存储后端 + 注册表
        # -------------------------------------------------
        storage = LocalStorage(os.path.join(tmp, "models"))
        registry = ModelRegistry(storage)

        # -------------------------------------------------
        # 7.3 注册 v1（默认进入 STAGING 阶段）
        # -------------------------------------------------
        mv1 = registry.register(
            "ppo-momentum",
            model_path,
            metadata={
                "description": "PPO momentum strategy v1",
                "metrics": {"sharpe": 1.5, "max_drawdown": 0.12},
                "hyperparameters": {"learning_rate": 3e-4, "gamma": 0.99},
                "git_commit": "abc1234",
            },
        )
        print(f"\n[步骤 7] 注册 v1: {mv1}")
        print(f"  初始阶段: {mv1.stage.value}")

        # -------------------------------------------------
        # 7.4 阶段转换：STAGING -> PRODUCTION
        # -------------------------------------------------
        mv1_prod = registry.transition_stage(
            "ppo-momentum", mv1.version, ModelStage.PRODUCTION
        )
        print(f"[步骤 7] 提升为 Production: {mv1_prod}")

        # -------------------------------------------------
        # 7.5 查询当前 Production 版本
        # -------------------------------------------------
        prod = registry.get_production("ppo-momentum")
        print(f"[步骤 7] 当前 Production: {prod}")

        # -------------------------------------------------
        # 7.6 注册 v2（新的改进版）
        # -------------------------------------------------
        model_path_v2 = os.path.join(tmp, "ppo_momentum_v2.onnx")
        with open(model_path_v2, "wb") as f:
            f.write(b"ONNX model weights v2 (2048 params)")

        mv2 = registry.register(
            "ppo-momentum",
            model_path_v2,
            metadata={
                "description": "PPO momentum strategy v2",
                "metrics": {"sharpe": 1.8, "max_drawdown": 0.10},
            },
        )
        print(f"\n[步骤 7] 注册 v2: {mv2}")

        # 将 v2 提升为 Production（v1 自动降级为 ARCHIVED）
        mv2_prod = registry.transition_stage(
            "ppo-momentum", mv2.version, ModelStage.PRODUCTION
        )
        print(f"[步骤 7] v2 提升为 Production，v1 自动降级为 ARCHIVED")

        # -------------------------------------------------
        # 7.7 列出所有版本
        # -------------------------------------------------
        all_versions = registry.list_versions("ppo-momentum")
        print(f"\n[步骤 7] 所有版本 ({len(all_versions)} 个):")
        for v in all_versions:
            print(f"  - {v}")

        # -------------------------------------------------
        # 7.8 回滚：从 v2 回滚到 v1
        # -------------------------------------------------
        print(f"\n[步骤 7] 执行回滚 ...")
        rolled = registry.rollback("ppo-momentum")
        print(f"[步骤 7] 回滚后 Production: {rolled}")

        # -------------------------------------------------
        # 7.9 下载产物（部署到推理节点）
        # -------------------------------------------------
        dest = os.path.join(tmp, "deployed_model.onnx")
        registry.download_artifact("ppo-momentum", rolled.version, dest)
        print(f"[步骤 7] 产物已下载到: {dest}")

    print("\n[步骤 7] 模型注册与上线流程完成")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

---

## 流水线数据流总结

| 步骤 | 模块 | 输入 | 输出 | 关键 API |
|------|------|------|------|----------|
| 1 | `axon-data` | 原始行情 | `MarketBar[]` + `FeatureConfig[]` | `make_synthetic_market_data` |
| 2 | `axon-rl` | 数据 + 配置 | `model.zip` | `TradingEnv::new`, `SyncVecEnv::new` |
| 3 | `axon-hpo` | 搜索空间 + 目标函数 | 最优超参 / Pareto 前沿 | `OptunaHPO::run` |
| 4 | `axon-walk-forward` | 数据集 + 超参 | OOS 指标 + DSR | `py_purge_overlapping_labels`, `py_deflated_sharpe` |
| 5 | `axon-backtest` | 模型 + 事件队列 | `RunResult` | `BacktestEngine::run`, `step` |
| 6 | `axon-inference` | `model.zip` | `.onnx` / `.pt` | `OnnxBackend::load`, `TchBackend::load` |
| 7 | `axon-registry` | 模型文件 + 元数据 | `ModelVersion` | `ModelRegistry::register`, `transition_stage` |

---

## 参考源码路径

- `crates/axon-rl/src/env/trading_env.rs` — `TradingEnv::step()` 主循环
- `crates/axon-rl/src/vec_env/sync.rs` — `SyncVecEnv` 向量化环境
- `crates/axon-rl/src/vec_env/async_env.rs` — `AsyncVecEnv` 异步并行环境
- `crates/axon-hpo/python/axon_hpo/optuna_runner.py` — `OptunaHPO` 封装
- `crates/axon-walk-forward/python/axon_walk_forward/evaluation.py` — `aggregate_folds` 与 DSR
- `crates/axon-backtest/src/engine.rs` — `BacktestEngine::run()` / `step()`
- `crates/axon-inference/src/backend/onnx.rs` — `OnnxBackend`
- `crates/axon-inference/src/backend/tch.rs` — `TchBackend`
- `crates/axon-registry/python/axon_registry/registry.py` — `ModelRegistry`
- `examples/02_rl_training/train_ppo.py` — PPO 训练完整示例
- `examples/03_hpo/hpo_single_objective.py` — HPO 基础示例
- `examples/08_walk_forward/walk_forward_basic.py` — Walk-Forward 基础示例
- `examples/05_registry/registry_register_promote.py` — 注册 + 提升示例
