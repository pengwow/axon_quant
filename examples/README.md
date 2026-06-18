# AXON Quant 示例

本目录包含 AXON Quant 的使用示例，按功能分类组织。

**约定**：所有示例使用 `_common.py` 工具层加载 `axon_rl` Rust 扩展。

## 目录结构

```
examples/
├── 01_getting_started/      # 入门示例
├── 02_rl_training/          # RL 训练示例
├── 03_hpo/                  # 超参数优化示例
├── 04_distributed/          # 分布式训练示例
├── 05_registry/             # 注册表示例
├── 06_tracker/              # 指标追踪示例
├── 07_visualization/        # 可视化示例
├── 08_walk_forward/         # Walk-Forward 验证示例
├── _common.py               # 共享工具层
├── _vec_env.py              # VecEnv 包装层
└── README.md                # 本文档
```

## 示例列表

### 01_getting_started - 入门示例

| 示例 | 说明 | 依赖 |
|------|------|------|
| `01_quick_start.py` | 快速入门：创建环境、运行随机策略、观察交互 | axon_rl (Rust 扩展) |
| `02_data_analysis.py` | 策略分析：多种策略运行、性能指标计算、对比排名 | axon_rl, numpy |
| `03_strategy_backtest.py` | 策略回测：动量/均值回归/RSI 策略实现与回测 | axon_rl, numpy |

### 02_rl_training - RL 训练示例

| 示例 | 说明 | 依赖 |
|------|------|------|
| `random_agent.py` | 随机策略基线（5 episodes × 500 步） | 零依赖 |
| `custom_reward.py` | PnL / Sharpe / Sortino 三种奖励函数对比 | 零依赖 |
| `train_ppo.py` | PPO 训练（与随机基线对比） | stable-baselines3, torch |
| `train_sac.py` | SAC 训练（连续动作空间） | stable-baselines3, torch |
| `vec_env_train.py` | 向量化环境训练（n_envs 对比） | stable-baselines3, torch |

### 03_hpo - 超参数优化示例

| 示例 | 说明 | 依赖 |
|------|------|------|
| `hpo_single_objective.py` | 单目标 HPO 示例（Optuna） | axon-hpo |
| `hpo_smoke_test.py` | HPO 冒烟测试 | axon-hpo |

### 04_distributed - 分布式训练示例

| 示例 | 说明 | 依赖 |
|------|------|------|
| `distributed_basic.py` | 分布式基础示例 | axon-distributed |
| `distributed_actor_pool.py` | Actor 池示例 | axon-distributed |

### 05_registry - 注册表示例

| 示例 | 说明 | 依赖 |
|------|------|------|
| `registry_register_promote.py` | 注册与升级示例 | axon-registry |
| `registry_rollback.py` | 回滚示例 | axon-registry |

### 06_tracker - 指标追踪示例

| 示例 | 说明 | 依赖 |
|------|------|------|
| `tracker_basic.py` | 指标追踪基础示例 | axon-tracker |
| `tracker_multi_backend.py` | 多后端指标追踪示例 | axon-tracker |

### 07_visualization - 可视化示例

| 示例 | 说明 | 依赖 |
|------|------|------|
| `visualize.py` | 回测结果可视化（净值曲线 + 回撤 + 交易信号） | matplotlib, numpy |

### 08_walk_forward - Walk-Forward 验证示例

| 示例 | 说明 | 依赖 |
|------|------|------|
| `walk_forward_basic.py` | Walk-Forward 基础示例 | axon-walk-forward |
| `walk_forward_purging.py` | Purged Walk-Forward 示例 | axon-walk-forward |

### 共享工具

| 文件 | 说明 |
|------|------|
| `_common.py` | 共享工具层：合成数据生成、环境工厂、随机策略 |
| `_vec_env.py` | Gymnasium / sb3 VecEnv 包装层（支持 AsyncVectorEnv） |

## 运行方法

```bash
# 激活虚拟环境
source .venv/bin/activate

# 入门示例
python examples/01_getting_started/01_quick_start.py
python examples/01_getting_started/02_data_analysis.py
python examples/01_getting_started/03_strategy_backtest.py

# RL 训练示例（零依赖）
python examples/02_rl_training/random_agent.py
python examples/02_rl_training/custom_reward.py

# RL 训练示例（需要 sb3 + torch）
pip install stable-baselines3 gymnasium torch
python examples/02_rl_training/train_ppo.py --timesteps 5000 --n-envs 1
python examples/02_rl_training/train_sac.py --timesteps 5000 --reward sharpe
python examples/02_rl_training/vec_env_train.py --n-envs 4 --timesteps 5000

# HPO 示例
python examples/03_hpo/hpo_single_objective.py
python examples/03_hpo/hpo_smoke_test.py

# 分布式示例
python examples/04_distributed/distributed_basic.py
python examples/04_distributed/distributed_actor_pool.py

# 注册表示例
python examples/05_registry/registry_register_promote.py
python examples/05_registry/registry_rollback.py

# 追踪示例
python examples/06_tracker/tracker_basic.py
python examples/06_tracker/tracker_multi_backend.py

# 可视化示例
pip install matplotlib numpy
python examples/07_visualization/visualize.py --n-bars 500 --show

# Walk-Forward 示例
python examples/08_walk_forward/walk_forward_basic.py
python examples/08_walk_forward/walk_forward_purging.py
```

## 依赖安装

```bash
# 基础依赖
pip install axon-quant

# RL 训练依赖
pip install stable-baselines3 gymnasium torch

# 可视化依赖
pip install matplotlib numpy
```

## 设计原则

1. **零外部数据依赖**：所有 RL 示例使用 `_common.make_synthetic_market_data` 生成合成 K 线
2. **零强制依赖**：基线示例（`random_agent.py` / `custom_reward.py`）不依赖 numpy/torch/sb3
3. **优雅降级**：若可选依赖不可用，提示用户安装并退出
4. **可配置**：通过 CLI 参数调整训练步数、并行环境数等
5. **按功能分类**：示例按使用场景组织到不同子目录，便于查找和学习
