# RL 训练用户指南(0.9.0)

本指南涵盖 AXON 0.9.0 发布的 RL 环境封装、策略抽象、ONNX 部署与 HPO 训练(`BacktestEnv` / `MultiLegBacktestEnv` / `L3BookDiff` 流式订阅 / `OnnxPolicyStrategy` / `RLHPOSweeper`)。

---

## 快速开始

```bash
# 1. 安装 RL 依赖
uv pip install "axon-quant[rl,onnx]"

# 2. 训练 spot 单 leg PPO 50K(SB3 路径)
uv run python examples/rl/train_spot_single_leg.py

# 3. 训练 spot+perp 套利 100K(主验收 demo)
uv run python examples/rl/spot_perp_arb_demo.py

# 4. 8-CPU 并发 HPO sweep(100 trial)
uv run python examples/rl/hpo_spot_perp_demo.py --n-trials 100 --n-jobs 8
```

---

## 核心概念

AXON 的 RL 训练把 **回测引擎**、**L3 订单簿流**、**PyO3 绑定**、**SB3/RLLib 训练**、**ONNX 部署**、**Optuna HPO** 串成一条端到端 pipeline。

```
   ┌─────────────────┐  env.step   ┌──────────────┐  export  ┌─────────┐
   │  BacktestEngine │ ────────────│  SB3/RLLib   │ ────────│  ONNX   │
   │  (Rust 内核)     │             │  (训练 loop)  │         │ policy  │
   └─────────────────┘             └──────────────┘         └─────────┘
          │                              │                      │
          │  L3BookDiff (per_bar)        │                      │
          │  ──────────────────►         │                      ▼
          │                              │            ┌───────────────────┐
          │                              │            │ OnnxPolicyStrategy│
          │                              │            │ (Python 部署)     │
          │                              │            └───────────────────┘
          ▼                              ▼                      │
   ┌─────────────────┐  best_params  ┌──────────────┐           │
   │  OptunaHPO      │ ─────────────│  RLHPOSweeper│           │
   │  (8-CPU 并发)    │             │  (Python 胶水) │           │
   └─────────────────┘             └──────────────┘           │
                                                            ▼
                                                ┌───────────────────┐
                                                │  BacktestEngine   │
                                                │  (生产部署 sim)    │
                                                └───────────────────┘
```

---

## API 概览

### `BacktestEnv`(D1.1)

包装 `BacktestEngine` 为 `gym.Env` 协议,单 leg 训练场景。

```python
from axon_quant.backtest import spot_instrument
from axon_quant.env import BacktestEnv

spot = spot_instrument("BTC", "USDT")
env = BacktestEnv(spot, seed=42)
obs, info = env.reset(seed=42)
obs, reward, term, trunc, info = env.step(env.action_space.sample())
```

**字段说明**:
- `observation_space`: `Box(shape=(OBS_DIM_SINGLE_LEG,))` —— 包含 last mid price、成交量、cash、position 等
- `action_space`: `Box(low=-1.0, high=1.0, shape=(1,))` —— 归一化目标仓位
- `reset()`: 重置 `BacktestEngine` + 跑第一根 bar 构造 obs
- `step(action)`:把 action 翻译为 order → engine.run() → 下一 bar obs + PnL reward

### `MultiLegBacktestEnv`(D1.2)

多 leg 同步(2-3 leg,主 demo 用 2 leg:spot + perp 套利)。

```python
from axon_quant.backtest import spot_instrument, swap_instrument
from axon_quant.env import MultiLegBacktestEnv

spot = spot_instrument("BTC", "USDT")
perp = swap_instrument("BTC", "USDT")
env = MultiLegBacktestEnv(
    [(spot, 1.0), (perp, 1.0)],
    seed=42,
)
```

各 leg observation 拼接为 `(OBS_DIM_SINGLE_LEG * n_legs,)` 的 `Box`,action 也是各 leg 拼接。

### `L3BookDiff` 流式订阅(C2.1,0.9.0 新增)

订阅 L3 订单簿增量,用于训练可视化、CB 监控、shadow strategy 验证。

```python
from axon_quant.backtest import BacktestEngine

engine = BacktestEngine(initial_cash=100_000.0)

def my_callback(diff):
    print(f"L3 diff @ {diff['timestamp_ns']}: +{len(diff['added'])} -{len(diff['removed'])}")

sub_id = engine.subscribe(callback=my_callback, kind="per_bar")
# ... 跑 sim ...
engine.unsubscribe(sub_id)
```

**`kind` 选项**:
- `"per_bar"`:每根 bar 结束时推 diff(训练可视化常用)
- `"per_fill"`:每笔成交时推 diff(高频回放 / 微观结构分析)
- `"both"`:两个时机都推(谨慎使用,可能重复)

### `BaseStrategy` ABC(C3.1)

Python 端策略抽象,镜像 Rust 侧 `StreamingStrategy` trait。

```python
from axon_quant.strategy import BaseStrategy

class MyStrategy(BaseStrategy):
    def on_bar(self, bar, ctx):
        # 必须实现:接收 bar + ctx(BarContext),返回 order list
        return []

    def on_fill(self, fill, ctx):
        # 可选:fill 触发,默认空实现
        return []
```

### `OnnxPolicyStrategy`(D1.4c)

部署时:加载 ONNX policy → BacktestEngine 决策。

```python
from pathlib import Path
from axon_quant.strategy import OnnxPolicyStrategy

strategy = OnnxPolicyStrategy(
    onnx_path=Path("artifacts/spot_perp_arb.onnx"),
    leg_specs=[(spot, 1.0), (perp, 1.0)],
    providers=["CPUExecutionProvider"],  # or ["CUDAExecutionProvider"]
)
action = strategy.predict(obs_sample)  # shape = (n_legs,)
```

### `RLHPOSweeper`(D1.5a)

Optuna HPO 胶水,8-CPU 并发 100 trial。

```python
from axon_quant.training import RLHPOSweeper

sweeper = RLHPOSweeper(
    study_name="my_hpo",
    n_trials=100,
    n_jobs=8,                              # 8-CPU 并发
    storage="sqlite:///optuna.db",         # 跨进程同步
)
best = sweeper.sweep(objective_fn=my_objective)
print(f"best params: {best}")
```

---

## 自定义 HPO 搜索空间

默认搜索空间为 PPO 4 维(lr/gamma/clip_param/entropy_coeff)。自定义:

```python
from axon_hpo.search_space import SearchSpaceDef
from axon_quant.training import RLHPOSweeper

custom_space = {
    "lr": SearchSpaceDef(param_type="log_uniform", low=1e-5, high=1e-3),
    "n_steps": SearchSpaceDef(param_type="categorical", choices=[512, 1024, 2048, 4096]),
    "batch_size": SearchSpaceDef(param_type="categorical", choices=[32, 64, 128, 256]),
    "gae_lambda": SearchSpaceDef(param_type="uniform", low=0.9, high=0.99),
}

sweeper = RLHPOSweeper(
    study_name="custom_hpo",
    n_trials=50,
    search_space=custom_space,
    n_jobs=4,
)
```

`SearchSpaceDef` 支持 `param_type`:
- `log_uniform`:对数均匀(适合 lr、entropy)
- `uniform`:线性均匀
- `categorical`:离散选项
- `int`:整数

---

## TensorBoard 集成

每次 trial 写到独立目录,便于多 trial 对比:

```python
from axon_quant.training.hpo_sweeper import make_tb_log_dir

def objective(params):
    tb_dir = make_tb_log_dir(trial_id=current_trial_id, base="./tb_logs")
    model = PPO("MlpPolicy", env, verbose=0, tensorboard_log=tb_dir, **params)
    model.learn(total_timesteps=50_000)
    return [sharpe_ratio]
```

启动 TensorBoard:

```bash
tensorboard --logdir ./tb_logs/
# 访问 http://localhost:6006
```

---

## 0.8.0 → 0.9.0 API 变化

| 0.8.0 | 0.9.0 (分支) | 变化 |
|-------|--------------|------|
| 无 `BacktestEnv` | `python/axon_quant/env.py` | 新增 `gym.Env` 包装 |
| 无 `L3BookDiff` | `BacktestEngine::subscribe()` | 新增流式订阅 |
| 无 `BaseStrategy` ABC | `python/axon_quant/strategy/base.py` | 新增 Python 策略抽象 |
| 无 `OnnxPolicyStrategy` | `python/axon_quant/strategy/onnx_policy.py` | 新增 ONNX 部署适配 |
| 无 `RLHPOSweeper` | `python/axon_quant/training/hpo_sweeper.py` | 新增 Optuna 胶水 |
| `Action`(5 类离散) | `MultiLegAction`(`axon-inference::types`) | 新增多 leg 连续动作 |

0.9.0 全部 19 个 plan 任务(详见 `docs/superpowers/plans/2026-07-22-axon-quant-0.9.0-rl-training.md`)。

---

## 主验收指标

| 指标 | 目标 | 失败标准 |
|------|------|----------|
| 训练收敛 | Sharpe > 1.0 (100K timesteps) | 100K 不收敛 → 调 reward / obs |
| HPO 增益 | best vs baseline Sharpe +20% | < 10% → 扩 search space |
| ONNX e2e | sim PnL 误差 < 5% | > 10% → 浮点 / schema 偏移 |
| HPO 性能 | 100 trial 8-CPU <= 3h | 超 3h → 缩 search space |

实际跑测结果待填(`docs/superpowers/notes/2026-07-22-rl-main-acceptance.md`)。

---

## 故障排查

| 症状 | 原因 | 修复 |
|------|------|------|
| `ImportError: stable_baselines3` | 没装 RL extra | `uv pip install axon-quant[rl]` |
| `ONNX export shape mismatch` | obs/action 维度不匹配 | 检查 `observation_space.shape == model.policy.obs_dim` |
| HPO trial 慢 | objective 内部实例化 env 太多 | env 改为 module-level,只在 `objective` 内 reset |
| L3BookDiff 不触发 | subscribe 在 `engine.run()` 之后 | 先 `subscribe` 再 `run` |
| `n_jobs > 1` 报 pickle 错误 | objective_fn 闭包含 unpicklable 对象 | 把状态挪到 module-level |
