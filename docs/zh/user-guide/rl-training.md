# RL 训练用户指南(0.9.0)

## 快速开始

```bash
# 1. 安装 RL 依赖
uv pip install "axon-quant[rl,onnx]"

# 2. 训练 spot 单 leg PPO 50K
uv run python examples/rl/train_spot_single_leg.py

# 3. 导出 ONNX + 部署到 BacktestEngine
uv run python examples/rl/spot_perp_arb_demo.py
```

## API 概览

### `BacktestEnv`

包装 `BacktestEngine` 为 `gym.Env` 协议。

```python
from axon_quant.backtest import spot_instrument
from axon_quant.env import BacktestEnv

spot = spot_instrument("BTC", "USDT")
env = BacktestEnv(spot, seed=42)
obs, info = env.reset(seed=42)
obs, reward, term, trunc, info = env.step(env.action_space.sample())
```

### `MultiLegBacktestEnv`

多 leg 同步(2-3 leg,主 demo 用 2 leg:spot + perp 套利)。

```python
from axon_quant.env import MultiLegBacktestEnv

env = MultiLegBacktestEnv([
    (spot, 1.0),
    (perp, 1.0),
], seed=42)
```

### `OnnxPolicyStrategy`

部署时:加载 ONNX policy → BacktestEngine 决策。

```python
from axon_quant.strategy import OnnxPolicyStrategy
strategy = OnnxPolicyStrategy(
    onnx_path=Path("artifacts/spot_perp_arb.onnx"),
    leg_specs=[(spot, 1.0), (perp, 1.0)],
)
```

### `RLHPOSweeper`

Optuna HPO 胶水,8-CPU 并发 100 trial。

```python
from axon_quant.training import RLHPOSweeper

sweeper = RLHPOSweeper(
    study_name="my_hpo",
    n_trials=100,
    n_jobs=8,
    storage="sqlite:///optuna.db",
)
best = sweeper.sweep(objective_fn=my_objective)
```

## 主验收指标

| 指标 | 目标 | 失败标准 |
|------|------|----------|
| 训练收敛 | Sharpe > 1.0 (100K timesteps) | 100K 不收敛 → 调 reward / obs |
| HPO 增益 | best vs baseline Sharpe +20% | < 10% → 扩 search space |
| ONNX e2e | sim PnL 误差 < 5% | > 10% → 浮点 / schema 偏移 |
| HPO 性能 | 100 trial 8-CPU <= 3h | 超 3h → 缩 search space |
