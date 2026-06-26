# 安装与快速入门

> **快速开始**: [`examples/01_getting_started/00_all_in_one.py`](https://github.com/pengwow/axon_quant/blob/main/examples/01_getting_started/00_all_in_one.py)
> 一键运行，覆盖全部 6 个 Stage，内置离线数据，无需额外配置。

> 本章节介绍如何在本地安装 AXON，并通过一个最小可运行的随机策略基线验证环境是否正常。

---

## 系统要求

AXON 采用 Rust + Python 双语言架构，需要以下环境：

| 组件 | 最低版本 | 说明 |
|------|---------|------|
| Python | 3.12 | 支持 3.12 / 3.13 / 3.14（`pyproject.toml` 限定 `<3.15`） |
| Rust | 1.96.0 | 通过 `rustup` 安装，详见 [rustup.rs](https://rustup.rs) |
| maturin | 1.0+ | Python Wheel 构建工具（`pip install maturin`） |

!!! warning "Rust 版本严格匹配"
    AXON 使用 Rust 2024 Edition，要求编译器版本 `>= 1.96.0`。请通过 `rustc --version` 确认，若版本过低请执行 `rustup update`。

!!! note "操作系统支持"
    - Linux / macOS：完整支持（含 CPU 亲和性绑定）
    - Windows：编译期拒绝亲和性模块，建议使用 WSL2 或 numactl 替代

---

## 三种安装方式

### 方式一：PyPI 安装（推荐，仅 Python 用户）

AXON 通过 `maturin` 构建为 Python 原生扩展（`axon_quant` 包），安装后可直接在 Python 中 `import axon_quant`。

```bash
# 1. 确保 Python >= 3.12
python --version   # 应输出 Python 3.12.x 或更高

# 2. 安装 axon_quant（包含所有 Rust 扩展）
pip install axon-quant

# 3. 验证安装
python -c "import axon_quant; print(axon_quant.__version__)"
# 预期输出: 0.2.0
```

!!! note "PyPI 包名说明"
    Python 包名为 `axon-quant`（PyPI 规范使用连字符），导入时使用下划线：`import axon_quant`。

### 方式二：源码构建（开发者，需要 Rust 工具链）

如果你需要修改 Rust 源码、调试 Crate 或贡献代码，建议从源码构建。

```bash
# 1. 克隆仓库
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# 2. 编译 Rust Workspace（验证环境）
cargo build

# 3. 运行 Rust 测试集（1200+ 用例）
cargo test --workspace

# 4. 构建 Python Wheel
maturin build --release

# 5. 安装生成的 Wheel
pip install target/wheels/axon_quant-*.whl

# 6. 验证
python -c "import axon_quant; print(axon_quant.__version__)"
```

!!! tip "增量编译加速"
    Workspace 在 `Cargo.toml` 中配置了统一的 `target` 目录与 `resolver = "2"`，首次编译后增量编译通常只需数秒。

### 方式三：可选依赖（按需安装）

AXON 的核心 Python 依赖仅包含 `numpy`、`pandas`、`pyarrow`。根据使用场景，你可能需要额外安装：

| 场景 | 安装命令 |
|------|---------|
| RL 训练（PPO / SAC 等） | `pip install stable-baselines3 gymnasium torch` |
| 超参优化（HPO） | `pip install optuna` |
| 实验追踪 | `pip install mlflow wandb` |
| 分布式训练 | `pip install ray` |
| 开发调试 | `pip install pytest pytest-cov ruff mypy` |

!!! note "依赖隔离建议"
    建议使用 `venv` 或 `conda` 创建独立环境，避免与系统 Python 包冲突：
    ```bash
    python -m venv .venv
    source .venv/bin/activate
    pip install -e ".[dev]"
    ```

---

## 快速入门代码示例

以下示例演示如何：
1. 创建合成市场数据（无需外部文件）
2. 初始化 `TradingEnv` 交易环境
3. 运行 `reset` → `step` 交互循环

```python
#!/usr/bin/env python3
"""
AXON 快速入门示例

演示 TradingEnv 的核心 API：
- 创建环境（config + market_data + reward）
- reset() 获取初始观测
- step(action) 执行交易并返回 (obs, reward, terminated, truncated, info)
"""

import axon_quant

# ── 1. 准备合成行情数据 ────────────────────────────────
# 无需外部 CSV/Parquet，直接生成 100 根几何布朗运动 K 线
market_data = [
    {
        "timestamp": i,               # 时间戳（任意递增整数）
        "open": 100.0 + i * 0.1,      # 开盘价
        "high": 100.5 + i * 0.1,      # 最高价
        "low": 99.5 + i * 0.1,        # 最低价
        "close": 100.0 + i * 0.1,     # 收盘价
        "volume": 1000.0,             # 成交量
    }
    for i in range(100)
]

# ── 2. 创建交易环境 ────────────────────────────────────
env = axon_quant.rl.TradingEnv(
    # 环境配置：初始资金、交易成本、最大步数等
    config={
        "initial_capital": 100_000.0,   # 初始资金 10 万
        "transaction_cost": 0.001,      # 交易成本 10 bps
        "slippage": 0.0005,             # 滑点 5 bps
        "max_steps": 100,               # 每回合最多 100 步
        "seed": 42,                     # 随机种子（保证可复现）
        "symbol": "BTCUSDT",            # 交易标的
        "return_window": 20,            # 收益率历史窗口（用于 Sharpe 奖励）
    },
    # 动作空间：连续动作，单维目标仓位比例 [-1, 1]
    #   1.0  = 全仓做多
    #   0.0  = 空仓
    #   -1.0 = 全仓做空
    action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    # 行情数据（必填）
    market_data=market_data,
    # 奖励函数："pnl"（盈亏）/ "sharpe"（夏普比率）/ "sortino"（索提诺比率）
    reward="pnl",
)

# ── 3. 重置环境，获取初始观测 ──────────────────────────
obs = env.reset()
print("初始观测特征:", obs["features"])       # 归一化后的 close + volume
print("特征名称:", obs["feature_names"])      # ["close", "volume"]

# ── 4. 执行一步交易 ────────────────────────────────────
# 动作：做多 50% 仓位（连续动作空间下传入 list[float]）
action = [0.5]
obs, reward, terminated, truncated, info = env.step(action)

print(f"\n执行 action={action} 后:")
print(f"  reward      = {reward:.4f}")               # 本步奖励（PnL 变化）
print(f"  portfolio   = {info['portfolio_value']:.2f}")  # 当前组合市值
print(f"  trades      = {info['trades_executed']}")       # 已成交笔数
print(f"  step        = {info['current_step']}")           # 当前步数
print(f"  done        = {info['done']}")                   # 回合是否结束

# ── 5. 渲染环境状态（ASCII 文本）────────────────────────
print("\n环境状态:", env.render())
# 输出示例: step=1/100 | value=$100000.00 | cash=$50000.00 | pos=0.5000 | trades=1 | cost=$50.00 | done=false
```

!!! note "观测空间默认配置"
    `TradingEnv` 默认使用 `close`（收盘价，Z-Score 归一化）+ `volume`（成交量，不归一化）作为观测特征。如需自定义特征，可在 Rust 层通过 `FeatureConfig` 扩展，未来版本将开放 Python API。

---

## 运行随机策略 Baseline 验证

随机策略是验证环境安装是否正确的最简方式：如果随机策略能稳定跑完多个 episode 而不崩溃，说明 `axon_quant` 的 Rust 扩展、Python 绑定、数据管道均已正常工作。

```python
#!/usr/bin/env python3
"""
随机策略基线验证

目的：
1. 端到端冒烟测试 —— 验证 Rust 扩展 + Python 接口 + 行情数据流通畅
2. 性能基线 —— 后续训练的策略应明显优于随机策略
3. CI 入口 —— 无需 GPU / 训练依赖即可运行
"""

import random
import axon_quant

# 生成 500 根合成 K 线（带随机游走 + 高斯噪声）
def make_synthetic_data(n=500, seed=42):
    rng = random.Random(seed)
    price = 100.0
    bars = []
    for t in range(n):
        open_p = price
        ret = rng.gauss(0.0, 0.01)          # 日收益率 ~ N(0, 1%)
        close = max(1e-6, open_p * (1.0 + ret))
        spread = abs(close - open_p) + open_p * 0.005
        high = max(open_p, close) + spread * rng.random()
        low = max(1e-6, min(open_p, close) - spread * rng.random())
        volume = abs(1000.0 + 200.0 * rng.gauss(0.0, 1.0))
        bars.append({
            "timestamp": t, "open": open_p, "high": high,
            "low": low, "close": close, "volume": volume,
        })
        price = close
    return bars


market_data = make_synthetic_data(n=500, seed=42)

# 创建环境
env = axon_quant.rl.TradingEnv(
    config={"initial_capital": 100_000.0, "max_steps": 500, "seed": 42},
    market_data=market_data,
    reward="pnl",
)

# 运行 5 个随机 episode
n_episodes = 5
records = []
for ep in range(n_episodes):
    env.reset()
    total_reward = 0.0
    steps = 0
    done = False
    rng = random.Random(ep)

    while not done and steps < 500:
        action = [rng.uniform(-1.0, 1.0)]   # 随机生成目标仓位 [-1, 1]
        obs, reward, terminated, truncated, info = env.step(action)
        total_reward += reward
        steps += 1
        done = terminated or truncated

    records.append({
        "episode": ep,
        "steps": steps,
        "total_reward": total_reward,
        "final_value": info["portfolio_value"],
    })
    print(f"Episode {ep}: steps={steps}, reward={total_reward:.4f}, "
          f"final_value={info['portfolio_value']:,.2f}")

# 汇总统计
mean_reward = sum(r["total_reward"] for r in records) / n_episodes
mean_value = sum(r["final_value"] for r in records) / n_episodes
print(f"\n=== 随机策略基线汇总 ===")
print(f"平均奖励: {mean_reward:.4f}")
print(f"平均净值: {mean_value:,.2f}")

# 验收：所有 episode 至少跑过 5 步即视为通过
completed = sum(1 for r in records if r["steps"] >= 5)
assert completed == n_episodes, f"仅 {completed}/{n_episodes} episodes 正常完成"
print("✅ 验收通过：AXON 环境运行正常！")
```

!!! tip "预期输出"
    由于采用随机策略且市场数据为随机游走，各 episode 的 reward 和 final_value 会围绕初始资金 `100,000.0` 小幅波动。关键是**无崩溃、无异常、所有 episode 正常结束**。

---

## 常见问题

### Q: `import axon_quant` 报错 `ModuleNotFoundError`

!!! warning "排查步骤"
    1. 确认 Wheel 已正确安装：`pip list | grep axon`
    2. 确认 Python 版本 `>= 3.12`：`python --version`
    3. 若从源码构建，确认 `maturin build --release` 成功且无 Rust 编译错误
    4. 检查是否处于正确的虚拟环境中

### Q: Rust 编译报错 `edition 2024 is unstable`

!!! warning "解决方案"
    Rust 2024 Edition 需要 `>= 1.96.0`。执行：
    ```bash
    rustup update stable
    rustc --version   # 确认 >= 1.96.0
    ```

### Q: macOS 上编译提示 `ld: library not found for -lpython3.12`

!!! warning "解决方案"
    设置 `PYO3_PYTHON` 环境变量指向正确的 Python 解释器：
    ```bash
    export PYO3_PYTHON=$(which python3.12)
    maturin build --release
    ```

---

## 下一步

- [AI 原生核心设计](../user-guide/ai-native-design.md) — 理解 AXON 为何是"AI 原生"而非"AI 附加"
- 查看 `examples/` 目录获取更多示例：
    - `examples/01_getting_started/03_strategy_backtest.py` — 动量 / 均值回归 / RSI 策略回测对比
    - `examples/02_rl_training/train_ppo.py` — 使用 Stable-Baselines3 训练 PPO 策略
    - `examples/03_hpo/hpo_single_objective.py` — 超参数自动优化
