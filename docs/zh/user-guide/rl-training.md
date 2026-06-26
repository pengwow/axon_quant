# RL 训练指南

本指南介绍如何使用 axon_quant 的强化学习（RL）功能训练交易策略。

## 快速开始

### 1. 安装依赖

```bash
# 基础安装（仅需运行环境，无需训练）
pip install axon_quant

# 包含 RL 训练依赖（gymnasium, stable-baselines3, torch）
pip install axon_quant[rl]
```

### 2. 运行随机策略基线（无需 sb3）

```bash
cd axon
PYTHONPATH=examples .venv/bin/python examples/02_rl_training/random_agent.py
```

### 3. 运行 PPO 训练（需要 sb3）

```bash
PYTHONPATH=examples .venv/bin/python examples/02_rl_training/train_ppo.py \
    --timesteps 5000 --n-envs 1
```

---

## 环境配置

`TradingEnv` 通过 config 字典配置：

```python
import axon_quant

config = {
    "initial_capital": 100_000.0,   # 初始资金
    "transaction_cost": 0.001,      # 手续费率
    "slippage": 0.0001,             # 滑点
    "max_steps": 500,               # 最大步数
    "seed": 42,                     # 随机种子
    "symbol": "BTCUSDT",           # 交易对
    "return_window": 50,            # 收益窗口（用于 Sharpe/Sortino）
}

env = axon_quant.rl.TradingEnv(
    config=config,
    action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    market_data=market_data,
    reward="pnl",
)
```

### 参数说明

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `initial_capital` | float | 100000 | 初始资金 |
| `transaction_cost` | float | 0.001 | 手续费率（0.1%） |
| `slippage` | float | 0.0001 | 滑点（0.01%） |
| `max_steps` | int | 500 | 每个 episode 最大步数 |
| `seed` | int | 42 | 随机种子（可复现） |
| `symbol` | str | "BTCUSDT" | 交易对名称 |
| `return_window` | int | 50 | Sharpe/Sortino 计算窗口 |

---

## 奖励函数

axon_quant 内置三种奖励函数：

### pnl — 绝对 PnL

```python
env = axon_quant.rl.TradingEnv(config=config, reward="pnl", ...)
```

- 计算：每步资金净值变化
- 适用：简单直观，适合初学者
- 特点：不考虑风险，可能产生高波动策略

### sharpe — 滚动夏普比率

```python
env = axon_quant.rl.TradingEnv(config=config, reward="sharpe", ...)
```

- 计算：滚动窗口内的夏普比率
- 适用：风险调整后收益优化
- 特点：默认 `clip=10.0`，防止极端值导致梯度爆炸

### sortino — 滚动索提诺比率

```python
env = axon_quant.rl.TradingEnv(config=config, reward="sortino", ...)
```

- 计算：仅考虑下行风险的收益比率
- 适用：更关注亏损风险的场景
- 特点：对上行波动不惩罚

### 选择建议

| 场景 | 推荐奖励 |
|------|----------|
| 快速验证 | `pnl` |
| 稳健策略 | `sharpe` |
| 风险厌恶 | `sortino` |

---

## 与 stable-baselines3 集成

### PPO 训练示例

```python
from stable_baselines3 import PPO
from axon_examples.vec_env import AxonTradingEnv, make_vec_env
from axon_examples.common import make_env, make_env_config, make_synthetic_market_data

# 1. 准备数据
market_data = make_synthetic_market_data(n=500, seed=42)
config = make_env_config(max_steps=500, seed=42)

# 2. 创建环境工厂
def env_fn():
    return AxonTradingEnv(make_env(config=config, market_data=market_data, reward="sharpe"))

# 3. 创建向量化环境
venv = make_vec_env(env_fn, n_envs=4, use_stable_baselines3=True)

# 4. 创建模型
model = PPO("MlpPolicy", venv, verbose=1, learning_rate=3e-4)

# 5. 训练
model.learn(total_timesteps=50_000)

# 6. 保存模型
model.save("ppo_trading")
```

### SAC 训练示例

```python
from stable_baselines3 import SAC

model = SAC(
    "MlpPolicy",
    venv,
    verbose=1,
    learning_rate=3e-4,
    buffer_size=10_000,
    batch_size=256,
)
model.learn(total_timesteps=50_000)
```

---

## 多环境并行训练

使用 `make_vec_env` 创建多个并行环境：

```python
from axon_examples.vec_env import make_vec_env

# 创建 4 个并行环境
venv = make_vec_env(env_fn, n_envs=4, use_stable_baselines3=True)

# 或使用异步环境（多进程）
venv = make_vec_env(env_fn, n_envs=4, use_async=True)
```

### 性能对比

```bash
# 运行对比实验
PYTHONPATH=examples .venv/bin/python examples/02_rl_training/vec_env_train.py \
    --n-envs 4 --timesteps 5000 --compare-with-serial
```

---

## 自定义奖励函数

当前奖励函数在 Rust 端实现，如需自定义可通过以下方式：

1. **修改 Rust 竺码**：在 `crates/axon-rl/src/reward/` 中添加新实现
2. **使用 Python 包装**：在 Python 端对 reward 做后处理

```python
class CustomRewardWrapper:
    """对原始 reward 做后处理。"""
    def __init__(self, env, alpha=0.5):
        self._env = env
        self._alpha = alpha
        self._prev_value = None

    def step(self, action):
        result = self._env.step(action)
        obs, reward, terminated, truncated, info = result
        # 自定义逻辑：结合 PnL 和持仓变化
        custom_reward = self._alpha * reward + (1 - self._alpha) * info.get("position_change", 0)
        return obs, custom_reward, terminated, truncated, info
```

---

## 完整训练流程

```python
"""完整的 PPO 训练 + 评估流程。"""
import time
from stable_baselines3 import PPO
from axon_examples.vec_env import AxonTradingEnv, make_vec_env
from axon_examples.common import (
    make_env, make_env_config, make_synthetic_market_data,
    run_random_episode, set_seed,
)

set_seed(42)

# 数据准备
market_data = make_synthetic_market_data(n=500, seed=42)
config = make_env_config(max_steps=500, seed=42)

def env_fn():
    return AxonTradingEnv(make_env(config=config, market_data=market_data, reward="pnl"))

# 训练
venv = make_vec_env(env_fn, n_envs=1)
model = PPO("MlpPolicy", venv, verbose=0, learning_rate=3e-4, n_steps=512)

t0 = time.perf_counter()
model.learn(total_timesteps=10_000)
print(f"训练耗时: {time.perf_counter() - t0:.1f}s")

# 评估
obs = venv.reset()
total_reward = 0
for _ in range(500):
    action, _ = model.predict(obs, deterministic=True)
    obs, reward, done, info = venv.step(action)
    total_reward += reward
    if done:
        break

print(f"策略累计奖励: {total_reward:.2f}")

# 对比随机策略
env = env_fn()
random_result = run_random_episode(env, max_steps=500, seed=42)
print(f"随机策略奖励: {random_result['total_reward']:.2f}")
```

---

## 常见问题

### Q: 出现 "缺少 RL 训练依赖" 提示

```bash
pip install gymnasium stable-baselines3 torch
```

或使用可选依赖：

```bash
pip install axon_quant[rl]
```

### Q: 训练速度慢

1. 增加并行环境数：`n_envs=4` 或更多
2. 使用 GPU：`pip install torch --index-url https://download.pytorch.org/whl/cu121`
3. 减少 `max_steps`：快速迭代验证

### Q: 如何使用真实数据

```python
import pandas as pd

# 从 CSV 读取
df = pd.read_csv("btc_1h.csv")
market_data = df[["timestamp", "open", "high", "low", "close", "volume"]].to_dict("records")

env = axon_quant.rl.TradingEnv(config=config, market_data=market_data, reward="sharpe")
```

### Q: 模型如何部署

```python
# 加载已训练模型
model = PPO.load("ppo_trading")

# 实时预测
obs = env.reset()
action, _ = model.predict(obs, deterministic=True)
```

---

## 相关文档

- [PPO 训练脚本](https://github.com/pengwow/axon_quant/blob/main/examples/02_rl_training/train_ppo.py)
- [SAC 训练脚本](https://github.com/pengwow/axon_quant/blob/main/examples/02_rl_training/train_sac.py)
- [向量化训练示例](https://github.com/pengwow/axon_quant/blob/main/examples/02_rl_training/vec_env_train.py)
- [奖励函数对比](https://github.com/pengwow/axon_quant/blob/main/examples/02_rl_training/custom_reward.py)
