# AXON 示例

本目录提供 AXON 强化学习模块的可运行示例。所有示例均以**零外部数据
依赖**为目标，使用 `examples/_common.py` 生成的合成 K 线（随机游走）
进行训练/回放验证。

## 目录结构

```
examples/
├── _common.py            # 共享工具：共享库探测 + 合成数据 + 环境工厂
├── _vec_env.py           # Gymnasium / DummyVecEnv 包装层
├── random_agent.py       # 随机策略基线（零第三方依赖）
├── custom_reward.py      # 自定义奖励函数对比（零第三方依赖）
├── train_ppo.py          # PPO 训练（依赖 stable-baselines3）
├── train_sac.py          # SAC 训练（依赖 stable-baselines3）
└── vec_env_train.py      # 向量化环境训练（依赖 stable-baselines3）
```

## Python 环境要求

由于 PyO3 与 Python 3.13 在 macOS 上的 GIL 兼容性问题（参考
`axon-design/01-tdd/02-phase1-rl/06-pyo3-bindings.md`），当前推荐使用
**macOS Framework Python 3.12**：

```bash
PY=/Library/Frameworks/Python.framework/Versions/3.12/bin/python3.12
$PY -m pip install stable-baselines3 gymnasium torch numpy
```

构建 Rust 扩展（仅需一次）：

```bash
cd axon
RUSTFLAGS="-C link-arg=-Wl,-rpath,/Library/Frameworks/Python.framework/Versions/3.12/lib" \
PYO3_PYTHON=$PY \
cargo build -p axon-rl --features python
```

## 运行示例

按从简单到复杂的顺序：

```bash
# 1. 零依赖：随机策略基线
$PY examples/random_agent.py

# 2. 零依赖：对比 3 种奖励函数
$PY examples/custom_reward.py

# 3. PPO 训练（需要 sb3）
$PY examples/train_ppo.py --timesteps 5000 --n-envs 1

# 4. SAC 训练（需要 sb3）
$PY examples/train_sac.py --timesteps 5000 --reward sharpe

# 5. 向量化环境训练（n_envs=4 vs 1 对比）
$PY examples/vec_env_train.py --n-envs 4 --timesteps 5000 --compare-with-serial
```

每个脚本：
- 退出码 0 = 通过；非 0 = 失败
- stdout 包含 PASS/FAIL 标记，便于 CI 抓取
- 不需要 GPU（PPO/SAC 在小规模 demo 上 CPU 足够）

## 常见问题

### 找不到 `axon_rl` 模块

`examples/_common.py:find_axon_rl_lib()` 会自动：
1. 在 `target/debug` 寻找 `libaxon_rl.dylib`
2. 在同目录创建 `axon_rl.cpython-XYZ-platform.so` 符号链接
3. 把 `target/debug` 加入 `sys.path`

如果提示 `libaxon_rl` 找不到，先 `cargo build -p axon-rl --features python`。

### GIL 错误

如果使用 `conda` / `miniconda` 的 Python 3.13，可能会触发
`PyInterpreterState_Get: GIL is released` 致命错误。解决方法：
1. 优先使用 macOS Framework Python 3.12（最稳定）
2. 升级到 `pyo3 0.23+`

## 设计原则

1. **零数据依赖**：`_common.make_synthetic_market_data` 生成随机 K 线，
   避免依赖外部 parquet / CSV。生产环境替换为真实数据时只需修改这一处。
2. **零强制依赖**：基线示例（`random_agent.py`、`custom_reward.py`）
   不依赖 `numpy` / `torch` / `stable-baselines3`，可作为 CI 入口。
3. **可对比**：`train_ppo.py` / `train_sac.py` 自动与 `random_agent.py`
   基线对比，方便评估训练收益。
4. **可扩展**：`make_env` / `make_vec_env` 提供统一工厂，未来新增策略
   或回测时复用同一套环境构造逻辑。

## 后续工作

- [ ] 添加 `backtest.py`：用训练好的 PPO/SAC 模型在样本外数据上回测
- [ ] 添加 `hpo_optuna.py`：用 Optuna 做 PPO 超参数搜索
- [ ] 添加 `visualize.py`：净值曲线 + 回撤 + 交易信号的可视化
- [ ] 完善 `gymnasium.vector.AsyncVectorEnv` 包装（多进程并行）
