# AXON 模块参考

> 本文档按 **crate 维度** 逐个描述工作区中每个模块的职责、机制、适用/不适用场景、代码位置与使用方法。
> 适合在「我应该用哪个 crate？」「这个特性在哪个文件？」等问题下作为索引使用。

## 阅读约定

每个模块章节统一包含 6 个字段：

| 字段 | 含义 |
|------|------|
| **核心职责** | 一句话说清楚这个模块做什么、解决什么问题 |
| **代码位置** | 关键文件 / 目录（相对仓库根目录） |
| **核心机制** | 内部实现的关键原理（数据结构 / 算法 / 并发模型） |
| **适用场景** | 哪些业务/工程场景下应该使用本模块 |
| **不适用场景** | 哪些场景用不到本模块（避免误用） |
| **怎么用** | 最少代码示例（**Python 为主，Rust 作为底层参考**） |
| **关键依赖** | 上游被依赖 + 下游依赖对象 |

---

## 1. `axon-core`

### 核心职责
工作区**最底层**类型库，定义整个系统共享的数据结构、错误约定、调度原语和统计原语。**不依赖**任何其他 `axon-*` crate。

### 代码位置
- `crates/axon-core/src/lib.rs` — 公开 re-export
- `crates/axon-core/src/time/` — `Timestamp` / `MonotonicClock` / `TimePrecision`
- `crates/axon-core/src/types/` — `Price` / `Quantity` / `Symbol`
- `crates/axon-core/src/market/` — `Tick` / `Bar` / `OrderBookSnapshot` / `Trade`
- `crates/axon-core/src/order/` — `Order` / `OrderType` / `TimeInForce` / `OrderStatus` 状态机
- `crates/axon-core/src/event/` — `Event` / `EventBuilder` / `EventRouter` / `EventHandler`
- `crates/axon-core/src/queue/` — `EventQueue`（按时间戳排序的优先队列）
- `crates/axon-core/src/portfolio/` — 多币种 `Portfolio` / `Position` / `TradeRecord`
- `crates/axon-core/src/scheduler/` — 模拟时钟 + 定时/周期任务
- `crates/axon-core/src/impact/` — 线性 / 幂律 / 自适应 / Almgren-Chriss 市场冲击模型
- `crates/axon-core/src/latency/` — 固定 / 正态 / 指数 / 均匀 / 队列延迟模型
- `crates/axon-core/src/volatility/` — EWMA / 滚动 / Garman-Klass 波动率
- `crates/axon-core/src/fee/` — 分级手续费表 + Maker/Taker 计费
- `crates/axon-core/src/metrics/` — 交易指标聚合
- `crates/axon-core/src/simd/` — SIMD 加速的归一化 / VaR / 订单簿
- `crates/axon-core/src/harness_types.rs` — `AgentIntent` / `TaskContext` / `HarnessResult`

### 核心机制
- **零依赖**：除 `Cargo.toml` 显式声明的 workspace 依赖外，不引入新 crate
- **#[repr(C)]** + 紧凑布局：例如 `Tick` 是 32 字节（i64 + 2 个 f64 + 1 字节 side + padding），便于 SIMD 加载
- **BinaryHeap + 时间戳**：事件队列是按 `(timestamp, seq)` 排序的小顶堆
- **状态机**：`OrderStatus` 状态转移在 `order/status.rs` 中以 `matches!` 守卫
- **serde 兼容**：所有跨边界数据可序列化

### 适用场景
- 实现自定义撮合、回测、撮合微结构模拟时复用 `EventQueue` + `Order`
- 研究市场冲击、滑点建模时用 `impact` 模块的 4 个 `ImpactModel` 实现
- 跨进程 / 跨语言传递订单与成交记录时使用 `Order` / `Trade` / `TradeRecord`
- 跑延迟敏感性实验时用 `latency` 模块给事件注入随机延迟

### 不适用场景
- 高频行情接入（应使用 `axon-exchange` / `axon-data`，它们依赖本模块但不直接调用）
- 业务策略实现（应使用 `axon-rl` / `axon-llm` / `axon-ensemble`，不直接操作 `EventQueue`）
- 部署时不再需要扩展（只读 `axon-core` 的类型即可，不要在业务层重新发明 `Order`）

### 怎么用

> `axon-core` 是 **Rust 内部基础库**，不直接暴露 Python 绑定。Python 用户通过
> `axon-data` / `axon-backtest` / `axon-oms` 等上层模块间接使用其中的 `Tick` /
> `Order` / `EventQueue` 等类型（dict 协议 / dataclass 映射）。需要直接使用
> Rust 内部类型时，调用方大多是 crate 自身或集成测试。

**Python 侧（最常见路径：通过 `axon_quant.data.Tick` / `axon_quant.backtest` 间接使用）：**

```python
from axon_quant.data import DataService, DataRequest, Frequency, MockSource
from axon_quant.backtest import limit_order, BacktestEngine

# 1) 通过 axon-data 拿 Tick 列表（内部就是 axon_core::market::Tick）
svc = DataService.new().register_source(
    MockSource.with_tick_series("btc", 1000, 1_000_000, lambda i: 100.0 + i)
)
ds = svc.load(DataRequest("BTCUSDT", "2026-01-01T00:00:00Z",
                          "2026-01-02T00:00:00Z", Frequency.Min1))

# 2) 通过 axon-backtest 构造 Order（内部就是 axon_core::order::Order）
bt = BacktestEngine(initial_cash=100_000.0)
bt.push_event({
    "type": "order_submitted",
    "timestamp_ns": 1_000,
    "order": limit_order(1, "BTCUSDT", "Buy", 100.0, 1.0),  # -> axon_core::order::Order::limit
})
result = bt.run()
```

**Rust 侧（开发新 crate / 集成测试时使用）：**

```rust
use axon_core::{EventQueue, Order, OrderType, Side, Price, Quantity, Tick, Timestamp};

let mut q = EventQueue::new();
q.push(Tick::new(Timestamp::from_nanos(1_000_000_000), 50_000.0, 0.1, Side::Buy));
let order = Order::limit("BTC-USDT".into(), Side::Buy, Price::from(50_000.0), Quantity::from(0.01));
```

### 关键依赖
- **被依赖**：几乎所有 `axon-*` crate（`axon-backtest` / `axon-rl` / `axon-oms` / `axon-risk` …）
- **不依赖**：任何 `axon-*`（这是设计约束）

---

## 2. `axon-backtest`

### 核心职责
事件驱动的回测引擎 + L1/L2/L3 多级确定性撮合 + 市场冲击感知撮合 + 流式回测。

### 代码位置
- `crates/axon-backtest/src/lib.rs` — 公开 API（`BacktestEngine` / `L1MatchingEngine` / `L2MatchingEngine`）
- `crates/axon-backtest/src/engine.rs` — 主循环（事件 → 撮合 → 成交 → 投资组合）
- `crates/axon-backtest/src/matching/l1.rs` — 价格-时间优先 Level 1 撮合
- `crates/axon-backtest/src/matching/l2.rs` — 多档价格 Level 2 撮合
- `crates/axon-backtest/src/matching/l3/` — Level 3：集合竞价 / 暗池 / 订单簿快照恢复
- `crates/axon-backtest/src/impact/` — `ImpactedMatchingEngine`（叠加 `ImpactModel` 的撮合包装）
- `crates/axon-backtest/src/streaming/` — 流式回测：`StreamingStrategy::on_tick` / `StrategyAction` / `ExchangeStreamSource` / `ReplayStreamSource`
- `crates/axon-backtest/src/python/` — PyO3 绑定（`axon_quant.backtest`）
- `crates/axon-backtest/tests/` — 17 个 e2e 集成测试

### 核心机制
- **撮合算法**：
  - L1（默认）：价格-时间优先的同价位队列
  - L2：保留多档价格深度，可支持修改（amend）
  - L3：包含集合竞价（开盘集合）、暗池撮合、订单簿快照/恢复
- **确定性**：单线程事件循环，无并发副作用；相同输入必产生相同输出
- **冲击注入**：`ImpactedMatchingEngine` 包装基础撮合，按 `ImpactModel::compute_impact(quantity, ...)` 调整成交价
- **流式回测**：策略实现 `StreamingStrategy::on_tick(&Tick, &OrderBook) -> StrategyAction`，由 `StreamingEngine` 驱动循环；`ExchangeStreamSource` 走 `crossbeam::channel`，`ReplayStreamSource` 从 CSV / Vec 回放

### 适用场景
- 任何需要可重现回测的策略研究（用 `BacktestEngine` + `L2MatchingEngine` 起步）
- 验证 RL 策略的样本外表现（`axon-rl::TradingEnv` 内部包装的就是本模块）
- 模拟真实成交滑点（`ImpactedMatchingEngine` + `LinearImpactModel`）
- 跑 Tick 级别的高频策略并用流式管线对接 live 数据（`streaming::engine.rs`）

### 不适用场景
- 真实下单（应使用 `axon-oms` + `axon-exchange`）
- 单标的、毫秒级以下的微结构回测（L3 撮合是工程近似，不是交易所级仿真）
- 跨标的组合优化（那是 `axon-ensemble` / `axon-hpo` 的领域）

### 怎么用

**Python 侧（主用法，绝大多数策略研究场景）：**

```python
from axon_quant.backtest import (
    BacktestEngine, L2MatchingEngine, ImpactedMatchingEngine,
    ImpactedMatchingEngineBuilder, limit_order, market_order,
)

# 1) 事件驱动回测：L1（默认）/ L2 撮合
bt = BacktestEngine(initial_cash=100_000.0)
bt.with_matching_engine(L2MatchingEngine())           # 可选：换成 L2
bt.with_seed_liquidity(half_spread=0.5, depth_levels=10, size_per_level=1.0)
bt.begin_bar(price=50_000.0, symbol="BTCUSDT")        # 每根 bar 必调
bt.push_event({
    "type": "order_submitted",
    "timestamp_ns": 1_000_000_000,
    "order": limit_order(1, "BTCUSDT", "Buy", 50_000.0, 0.1),
})
result = bt.run()
print(result.final_nav, result.fills)

# 2) 真实滑点模拟：叠加 ImpactModel
ie = (ImpactedMatchingEngineBuilder()
      .model_type("linear")
      .coefficient(0.1)
      .depth_levels(5)
      .build())
ie.submit(limit_order(2, "BTCUSDT", "Buy", 50_000.0, 0.1))

# 3) 也可直接调撮合引擎做单笔提交（不经过 BacktestEngine 主循环）
l2 = L2MatchingEngine()
fill = l2.submit(limit_order(3, "BTCUSDT", "Sell", 50_000.0, 0.1))
print(fill["is_filled"], fill["fills"])
```

**Rust 侧（开发新撮合算法 / 性能调优时使用）：**

```rust
use axon_backtest::{BacktestEngine, L2MatchingEngine};
use axon_core::Tick;

let mut engine = BacktestEngine::new(L2MatchingEngine::new("BTC-USDT".into()));
engine.feed_tick(Tick::new(/* ... */));
let result = engine.run_to_end()?;
println!("Sharpe = {}, MaxDD = {}", result.sharpe(), result.max_drawdown());
```

### 关键依赖
- **依赖**：`axon-core`（类型基础）
- **被依赖**：`axon-rl`（交易环境包装）、`axon-llm::trading`（回测工具）、各 `tests/` 集成测试

---

## 3. `axon-rl`

### 核心职责
Gymnasium 兼容的强化学习交易环境 + 多目标奖励 + 向量化并行 rollout。

### 代码位置
- `crates/axon-rl/src/lib.rs` — 入口与 re-export
- `crates/axon-rl/src/env/trading_env.rs` — `TradingEnv`（Gymnasium 5 元组接口）
- `crates/axon-rl/src/env/action_decoder.rs` — 动作 → 订单转换
- `crates/axon-rl/src/env/executor.rs` — 用 `axon-backtest` 执行下单
- `crates/axon-rl/src/observation/` — 特征工程（滑窗 / 归一化 / `BoxSpace` / `DiscreteActionSpace`）
- `crates/axon-rl/src/action/` — 动作空间（Discrete / Continuous / MultiDiscrete）+ 转换器 + 平滑器
- `crates/axon-rl/src/reward/` — 奖励函数（PnL / Sharpe / Sortino / MultiObjective / Scaled）
- `crates/axon-rl/src/vec_env/` — `SyncVecEnv` / `AsyncVecEnv` 并行 rollout
- `crates/axon-rl/src/python/` — PyO3 绑定（`axon_quant.rl`）

### 核心机制
- **环境状态机**：`TradingEnv` 内部维护 `current_step / market_state / portfolio_state`，`step(action)` 返回 `(obs, reward, terminated, truncated, info)`
- **动作空间**：`DiscreteActionConverter` 把 int 转 `(side, qty_bin, type)`；`ContinuousActionConverter` 把 `[-1, 1]` 转 `(direction, size_ratio)`
- **奖励函数**：`PnLReward` / `SharpeReward` / `SortinoReward` / `MultiObjectiveReward`（可加权组合）
- **并行 rollout**：`AsyncVecEnv` 用 tokio + `crossbeam-channel` 分发 step，`SyncVecEnv` 用 rayon

### 适用场景
- 训练 PPO / SAC / A2C 等 RL 智能体（用 `axon_quant.rl.TradingEnv` 在 Python 端接入 Stable-Baselines3 / RLlib）
- 多目标 RL 训练（用 `MultiObjectiveReward`，配 HPO 搜索权重）
- 在 CPU 上跑大规模并行 rollout（`AsyncVecEnv` 配 `num_envs=64`）
- 跑 baseline 策略（用 `DiscreteActionSpace` + 默认 reward 即可）

### 不适用场景
- 在线学习（环境是 offline 的，无 live 数据接入）
- 超低延迟（`TradingEnv` 一次 `step` 包含 1 次撮合 + 1 次投资组合更新 + 1 次特征计算，微秒级而非纳秒级）
- 不需要 RL 的规则策略（直接用 `axon-backtest::BacktestEngine` 即可）

### 怎么用

**Python 侧（主用法，对接 Stable-Baselines3 / RLlib 等 RL 库）：**

```python
import axon_quant
from stable_baselines3 import PPO

# 1) 创建交易环境（Gymnasium 5 元组接口）
env = axon_quant.rl.TradingEnv(
    config={"initial_capital": 100_000.0, "max_steps": 500},
    market_data=bars,                                  # np.ndarray / list[dict]
    action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    reward="sharpe",                                   # "sharpe" / "sortino" / "pnl" / "multi_objective"
)

# 2) 标准 RL 训练循环
obs, info = env.reset()
model = PPO("MlpPolicy", env, verbose=1)
model.learn(total_timesteps=10_000)

# 3) 推理 / 回放
obs, reward, terminated, truncated, info = env.step([0.5])
print(reward, info)  # info 里通常包含 sharpe / drawdown / position_size

# 4) 向量化并行 rollout（CPU 多核加速）
venv = axon_quant.rl.AsyncVecEnv(                      # 或 SyncVecEnv
    num_envs=64,
    env_fn=lambda: axon_quant.rl.TradingEnv(config={...}, market_data=bars),
)
model = PPO("MlpPolicy", venv, verbose=1)
```

**Rust 侧（开发新奖励 / 新动作空间时使用）：**

```rust
use axon_rl::TradingEnv;
use axon_core::market::Bar;

// 自己实现 FeaturePipeline + 注入到环境
let env = TradingEnv::builder()
    .bars(bars)
    .reward(SharpeReward::new(window=63))
    .action_space(ContinuousActionSpace::new(-1.0, 1.0))
    .build();
let (obs, info) = env.reset();
let (obs, reward, term, trunc, info) = env.step(&vec![0.5]);
```

### 关键依赖
- **依赖**：`axon-core`（类型）+ `axon-backtest`（撮合执行）
- **被依赖**：`axon-distributed`（RLlib 集成）、`axon-llm`（LLM 驱动 RL 决策）、`axon-tracker`（训练指标）

---

## 4. `axon-hpo`

### 核心职责
超参数优化工具链：搜索空间定义 + Optuna 集成（Python 端）+ 多目标 + Pareto 前沿 + 超体积。

### 代码位置
- `crates/axon-hpo/src/lib.rs` — 入口
- `crates/axon-hpo/src/config.rs` — `HPOConfig` / `SamplerConfig` / `PrunerConfig`
- `crates/axon-hpo/src/search_space.rs` — `SearchSpaceDef`（Uniform / LogUniform / Int / Categorical）
- `crates/axon-hpo/src/trial.rs` — `TrialResult` / `TrialState`
- `crates/axon-hpo/src/result.rs` — `HPOResult`
- `crates/axon-hpo/src/pareto.rs` — `ParetoFront` / `compute_hypervolume` / `dominates`
- `crates/axon-hpo/python/axon_hpo/` — Python 端 Optuna 适配器（`optuna_runner.py` / `multi_objective.py` / `pruning.py` / `search_space.py`）

### 核心机制
- **搜索空间**：`SearchSpaceDef` 用 enum 表达每种分布，序列化后传给 Optuna
- **多目标 Pareto**：`dominates(a, b)` 实现 Pareto 占优比较；`compute_hypervolume` 用 NSGA-II 风格的 WFG 算法
- **剪枝**：`MedianPruner` / `SuccessiveHalvingPruner` 提前终止表现差的 trial
- **Python 桥**：`python/axon_hpo/optuna_runner.py` 把 Rust 搜索空间转成 Optuna 的 `suggest_*` 调用

### 适用场景
- RL 策略超参搜索（学习率 / 折扣因子 / 网络结构）
- 策略参数调优（止盈止损阈值 / 仓位上限 / 信号窗口大小）
- 多目标 HPO（同时优化 Sharpe + MaxDD）
- 与 walk-forward 联合使用（`axon-integration-tests` 里有现成例子）

### 不适用场景
- 大模型微调（这是 HPO，不是 NAS，且 Rust 端没有 GPU 调度）
- 实时自适应（Optuna 是离线批跑，无在线学习）
- 离散决策树类问题（用 XGBoost 自己的 tuner 即可）

### 怎么用

**Python 侧（主用法，直接用 Optuna 跑搜索）：**

```python
from axon_hpo import HPORunner, SearchSpace, StudyConfig
import axon_quant

# 1) 定义搜索空间
space = (SearchSpace()
    .uniform("lr", 1e-5, 1e-2)
    .log_uniform("gamma", 0.9, 0.999)
    .categorical("activation", ["relu", "tanh"])
    .int_uniform("hidden_size", 64, 512, step=64))

# 2) 定义目标函数（用 axon_quant.rl 训练 + 评估）
def objective(trial_params: dict) -> float:
    env = axon_quant.rl.TradingEnv(config={**trial_params, "max_steps": 500},
                                   market_data=bars,
                                   action_space={"type": "continuous",
                                                 "min": -1.0, "max": 1.0})
    # 简化的训练：直接评估随机策略
    obs, _ = env.reset()
    sharpe = 0.0
    for _ in range(500):
        a = env.action_space.sample()
        _, r, term, trunc, info = env.step(a)
        sharpe = info.get("sharpe", 0.0)
        if term or trunc: break
    return sharpe

# 3) 跑搜索
study = HPORunner(study_config=StudyConfig(direction="maximize", n_trials=50))
best = study.run(space, objective_fn=objective)
print(best.params, best.value)

# 4) 多目标搜索（Pareto 前沿）
pareto_study = HPORunner(study_config=StudyConfig(
    directions=["maximize", "minimize"], n_trials=100))  # 第一个: sharpe, 第二个: maxdd
```

**Rust 侧（开发新 pruner / 嵌入训练 pipeline 时使用）：**

```rust
use axon_hpo::{HPOConfig, SamplerConfig, PrunerConfig, SearchSpaceDef};

let cfg = HPOConfig {
    n_trials: 50,
    sampler: SamplerConfig::Tpe,
    pruner: PrunerConfig::Median { warmup_steps: 5 },
};
let space = SearchSpaceDef::new()
    .uniform("lr", 1e-5, 1e-2)
    .log_uniform("gamma", 0.9, 0.999);
```

### 关键依赖
- **依赖**：`axon-core`
- **被依赖**：`axon-rl`（训练循环）、`axon-llm`（prompt 调优）、`axon-integration-tests`

---

## 5. `axon-walk-forward`

### 核心职责
时间序列专用的滚动 / 扩展窗口验证 + purge / embargo 防泄漏 + OOS 指标聚合 + Deflated Sharpe Ratio。

### 代码位置
- `crates/axon-walk-forward/src/lib.rs` — 入口
- `crates/axon-walk-forward/src/config.rs` — `WalkForwardConfig` / `WindowType`
- `crates/axon-walk-forward/src/split.rs` — `TimeSeriesSplitter`（Rolling / Expanding）
- `crates/axon-walk-forward/src/purge.rs` — `purge_overlapping_labels` / `embargo_indices` / `detect_leakage`
- `crates/axon-walk-forward/src/metrics.rs` — `FoldResult` / `ISMetrics` / `OOSMetrics` / `StabilityMetrics`
- `crates/axon-walk-forward/src/evaluation.rs` — `aggregate_folds` / `compute_deflated_sharpe`

### 核心机制
- **窗口分割**：`TimeSeriesSplitter::split(start, end)` 返回 `Vec<FoldSplit>`，每个含 train/val/test 索引
- **防泄漏**：
  - `purge_overlapping_labels` 移除训练集中标签与验证集重叠的样本
  - `embargo_indices` 在 train/val 边界加一段空白期
  - `detect_leakage` 报告潜在泄露的样本数
- **Deflated Sharpe**：`compute_deflated_sharpe` 校正多重检验偏差（参考 Bailey & López de Prado）

### 适用场景
- 评估策略的真实样本外表现
- 检测过拟合（Deflated Sharpe 显著低于普通 Sharpe）
- 滚动再训练（每月用前 12 个月重训一次）
- 与 `axon-hpo` + `axon-registry` 组成完整训练管线（见 `axon-integration-tests::e2e_pipeline`）

### 不适用场景
- IID 数据（应使用 sklearn 的 `KFold`，本模块是专门为时间序列设计的）
- 单次回测（没 fold 概念）
- 实时流（没有这个能力）

### 怎么用

**Python 侧（主用法，配合 RL / HPO 评估真实样本外表现）：**

```python
import axon_quant
from axon_quant.walk_forward import (
    WalkForwardConfig, WindowType, TimeSeriesSplitter,
    compute_deflated_sharpe, detect_leakage,
)
import numpy as np

# 1) 配置滚动窗口
cfg = WalkForwardConfig(
    window_type=WindowType.Rolling,
    train_window=252,           # 1 年交易日
    test_window=63,             # 3 个月
    step=63,                    # 步长 = 测试窗口
    embargo=5,                  # 5 日间隔防泄漏
)

# 2) 生成 fold 划分
splitter = TimeSeriesSplitter(cfg)
folds = splitter.split(start="2023-01-01", end="2025-01-01")
print(f"共 {len(folds)} 个 fold")  # 典型 9-10 个

# 3) 对每个 fold 跑回测 + 收集 OOS metrics
oos_returns = []
for fold in folds:
    train_bars = bars[fold.train_start:fold.train_end]
    test_bars  = bars[fold.test_start:fold.test_end]

    env = axon_quant.rl.TradingEnv(config={"initial_capital": 100_000},
                                   market_data=train_bars, reward="sharpe")
    # ... 在 train 上训练策略,在 test 上评估 ...
    oos_returns.append(fold_test_sharpe)

# 4) 防泄漏检查 + Deflated Sharpe
issues = detect_leakage(folds, bars)
print(f"潜在泄漏样本: {issues.count}")

deflated = compute_deflated_sharpe(
    sharpe_ratios=oos_returns, n_trials=50  # 校正 HPO 多重检验偏差
)
print(f"Deflated Sharpe = {deflated:.3f}")
# 若显著低于样本内 Sharpe,提示过拟合
```

**Rust 侧（开发新 splitter / 嵌入 axon-integration-tests 时使用）：**

```rust
use axon_walk_forward::{WalkForwardConfig, TimeSeriesSplitter, WindowType};

let cfg = WalkForwardConfig {
    window_type: WindowType::Rolling,
    train_window: 252,
    test_window: 63,
    step: 63,
    embargo: 5,
    ..Default::default()
};
let splits = TimeSeriesSplitter::new(cfg).split(start, end);
```

### 关键依赖
- **依赖**：`axon-core`
- **被依赖**：`axon-rl`（eval pipeline）、`axon-registry`（挑选 OOS 最优版本）、集成测试

---

## 6. `axon-cli`

### 核心职责
`axon` 命令行入口（Phase 0 仅打印 banner，后续阶段会接回测 / 训练 / 运行子命令）。

### 代码位置
- `crates/axon-cli/src/main.rs`

### 核心机制
- 极简 `fn main() -> Result<()>` 入口
- 用 `env!("CARGO_PKG_VERSION")` / `env!("RUSTC_VERSION")` / `target_triple` 等编译期常量

### 适用场景
- 当前阶段（0.4.0）：验证构建、版本号、平台信息
- 未来（plan 0.5+）：`axon backtest run` / `axon train ppo` / `axon serve` 等子命令的统一入口

### 不适用场景
- 现在的实际业务调用（功能尚未实装）
- 编程式 API（应使用 `axon-python` 绑定）

### 怎么用

```bash
$ cargo run -p axon-cli
axon 0.4.0
Rust 1.97.0 (aarch64-apple-darwin)
阶段：Phase 0 — 架构与基础设施
```

### 关键依赖
- **依赖**：`axon-core`（仅 `Result` 类型）
- **被依赖**：暂无

---

## 7. `axon-distributed`

### 核心职责
Ray / RLLib 分布式训练集群配置 + Actor / ParamServer / Checkpoint 容错。

### 代码位置
- `crates/axon-distributed/src/lib.rs` — 入口
- `crates/axon-distributed/src/config.rs` — `DistributedConfig` / `ClusterConfig` / `AlgorithmConfig` / `ResourceConfig` / `FaultToleranceConfig`
- `crates/axon-distributed/src/actor.rs` — `ActorConfig`
- `crates/axon-distributed/src/param_server.rs` — `ParamServerConfig`
- `crates/axon-distributed/src/checkpoint.rs` — `TrainingCheckpoint` / `StepMetrics` / `CheckpointMetadata`

### 核心机制
- **配置聚合**：`DistributedConfig` 是 4 个子 config 的聚合，序列化到 YAML 后交给 Ray
- **Checkpoint 链路**：`TrainingCheckpoint { step, metrics, model_state, optimizer_state }` 序列化为 Parquet/Arrow IPC，可被 RLLib 恢复
- **容错策略**：`FaultToleranceConfig` 控制重启次数 / 节点失联超时

### 适用场景
- 多机多卡跑 PPO / SAC（用 RLLib 调度）
- 大规模 HPO（每 trial 一个 actor）
- 长时间训练需要 checkpoint 续训
- `axon-integration-tests::distributed_flow` 给出了端到端例子

### 不适用场景
- 单机训练（开销大于收益）
- 需要纳秒级延迟的策略执行（网络开销太大）
- 部署推理服务（那是 `axon-inference` 的领域）

### 怎么用

**Python 侧（主用法，配合 Ray/RLlib 调度多 worker）：**

```python
from axon_quant.distributed import (
    DistributedConfig, ClusterConfig, ResourceConfig,
    FaultToleranceConfig, AlgorithmConfig, to_yaml, from_yaml,
)

# 1) 构造多机多卡集群配置
cfg = DistributedConfig(
    cluster=ClusterConfig(num_workers=8, num_gpus_per_worker=1),
    algorithm=AlgorithmConfig(name="PPO", framework="torch"),
    resources=ResourceConfig(memory_per_worker=16 * 1024**3),  # 16 GB
    fault_tolerance=FaultToleranceConfig(max_restarts=3, timeout_s=300),
)

# 2) 导出给 Ray / RLlib
yaml = to_yaml(cfg)
with open("cluster.yaml", "w") as f:
    f.write(yaml)

# 3) 在 RLlib trainer 里直接传 config dict
# (axon-rl 内部会自动 from_yaml 解析)
from ray.rllib.algorithms.ppo import PPOConfig
ppo_cfg = PPOConfig().environment("axon-trading-env").framework("torch")
ppo_cfg.resources(num_gpus=1)

# 4) Checkpoint 链：训练中保存 / 恢复
import axon_quant
ckpt = axon_quant.distributed.serialize_checkpoint(
    step=1000, metrics={"sharpe": 1.85}, model_state=model.state_dict())
axon_quant.distributed.save_checkpoint(ckpt, "/checkpoints/run1/step1000")
```

**Rust 侧（开发新容错策略 / 自定义 actor 时使用）：**

```rust
use axon_distributed::{DistributedConfig, ClusterConfig, ResourceConfig};

let cfg = DistributedConfig {
    cluster: ClusterConfig { num_workers: 8, num_gpus_per_worker: 1, .. },
    algorithm: Default::default(),
    resources: ResourceConfig { memory_per_worker: 16 * 1024 * 1024 * 1024, .. },
    fault_tolerance: Default::default(),
};
let yaml = serde_yaml::to_string(&cfg)?;
```

### 关键依赖
- **依赖**：`axon-core`
- **被依赖**：`axon-integration-tests`（distributed_flow 场景）、Python 端 RLLib adapter

---

## 8. `axon-tracker`

### 核心职责
统一 trait + 4 个后端的实验追踪：`Memory` / `Local` / `MLflow` / `WandB`。

### 代码位置
- `crates/axon-tracker/src/lib.rs` — 入口
- `crates/axon-tracker/src/tracker.rs` — `ExperimentTracker` trait
- `crates/axon-tracker/src/types.rs` — `MetricEntry` / `ParamValue` / `ArtifactInfo` / `RunStatus`
- `crates/axon-tracker/src/backends/memory.rs` — `MemoryTracker`（测试用）
- `crates/axon-tracker/src/backends/local.rs` — `LocalTracker`（本地 JSONL）
- `crates/axon-tracker/src/backends/mlflow.rs` — `MlflowTracker`（`http` feature）
- `crates/axon-tracker/src/backends/wandb.rs` — `WandbTracker`（`http` feature）
- `crates/axon-tracker/src/retry.rs` — `RetryPolicy`

### 核心机制
- **统一 trait**：`ExperimentTracker::start_run / log_metric / log_param / log_artifact / end_run`
- **MetricBuffer**：`TrackerBackend` 用 `MetricBuffer` 做批量 flush，降低 IO 频率
- **重试**：`RetryPolicy { max_attempts, backoff }` 包装 HTTP 后端的瞬时故障

### 适用场景
- 训练 RL 策略时记录每个 episode 的 reward / sharpe
- HPO 时记录每 trial 的超参与最终指标
- 把产物（模型 / 报告 / 配置）上传到 MLflow / W&B
- 用 `MemoryTracker` 跑单元测试（不依赖外部服务）

### 不适用场景
- 生产环境的实时告警（那是 `axon-monitor` 的领域）
- 跨 run 的大规模数据分析（应直接 query MLflow tracking server）
- 需要事务一致性的业务记账

### 怎么用

**Python 侧（主用法，4 个后端按需切换）：**

```python
from axon_quant.tracker import (
    MemoryTracker, LocalTracker, MlflowTracker, WandbTracker,
)

# 1) 单元测试：MemoryTracker（不依赖外部服务）
tracker = MemoryTracker()
run = tracker.start_run("ppo_btc_v1")
tracker.log_metric(run.id(), "sharpe", 1.85)
tracker.log_param(run.id(), "lr", 0.0003)
tracker.log_param(run.id(), "gamma", 0.99)
tracker.log_artifact(run.id(), "model.onnx")
tracker.end_run(run.id())

# 2) 本地持久化：LocalTracker（写入 JSONL）
local = LocalTracker(root_dir="/var/axon/runs")
run = local.start_run("ppo_btc_v1", tags={"env": "testnet"})

# 3) 远程：MLflow / W&B
mlf = MlflowTracker(tracking_uri="http://mlflow:5000", experiment="axon-rl")
run = mlf.start_run("ppo_btc_v1")
mlf.log_metric(run.id(), "sharpe", 1.85, step=1000)

wdb = WandbTracker(project="axon-rl", entity="my-team")
run = wdb.start_run("ppo_btc_v1", config={"lr": 0.0003})

# 4) 配合 RLlib 训练时按 step 上报
for step in range(total_steps):
    # ... 训练 ...
    tracker.log_metric(run.id(), "episode_reward", reward, step=step)
```

**Rust 侧（开发新 backend / 嵌入训练循环时使用）：**

```rust
use axon_tracker::{MemoryTracker, ExperimentTracker};

let mut tracker = MemoryTracker::new();
let run = tracker.start_run("ppo_btc_v1")?;
tracker.log_metric(run.id(), "sharpe", 1.85)?;
tracker.log_param(run.id(), "lr", 0.0003)?;
tracker.end_run(run.id())?;
```

### 关键依赖
- **依赖**：`axon-core`
- **被依赖**：`axon-rl`（训练）、`axon-hpo`（trial 记录）、`axon-registry`（关联 run id）

---

## 9. `axon-registry`

### 核心职责
模型注册表：版本管理 + 阶段生命周期（`staging` → `production`）+ 多后端存储 + 元数据签名。

### 代码位置
- `crates/axon-registry/src/lib.rs` — 入口
- `crates/axon-registry/src/registry.rs` — `ModelRegistry` 主结构
- `crates/axon-registry/src/types.rs` — `ModelVersion` / `ModelStage` / `ModelMetadata` / `SemVer`
- `crates/axon-registry/src/signature.rs` — `ModelSignature`（输入/输出张量描述）
- `crates/axon-registry/src/storage.rs` — `LocalStorage` / `StorageBackend` trait
- `crates/axon-registry/src/filter.rs` — `VersionFilter`

### 核心机制
- **三阶段**：`None` → `Staging` → `Production` → `Archived`；每次阶段转换记录审计
- **签名校验**：`ModelSignature` 记录输入 shape / dtype / 输出维度，加载时强制匹配
- **本地存储**：`LocalStorage` 把模型文件存到 `{root}/{model_name}/{version}/` 目录，JSON 元数据附在旁

### 适用场景
- 训练出多个 checkpoint，挑最优版本推到 `production`
- A/B 测试（同时保留 `production` 和 `challenger` 两个版本）
- 模型回滚（archived 版本可一键恢复）
- `axon-integration-tests::walkforward_registry` 演示了验证后自动注册

### 不适用场景
- 大规模分布式文件系统（应接 S3 / OSS，路线图见 `axon-registry` 的 storage 后端扩展）
- 模型训练本身
- 推理服务（加载后由 `axon-inference` 负责）

### 怎么用

**Python 侧（主用法，模型版本管理 + A/B 灰度）：**

```python
from axon_quant.registry import (
    ModelRegistry, ModelStage, LocalStorage, ModelSignature,
)

# 1) 构造本地存储后端 + 注册表
storage = LocalStorage(root_dir="/var/axon/models")
registry = ModelRegistry(storage=storage)

# 2) 注册一个新模型版本
version = registry.register_model(
    name="ppo_btc",
    version="1.0.0",
    model_bytes=open("model.onnx", "rb").read(),
    signature=ModelSignature(
        input_shape=(1, 64, 128),
        input_dtype="float32",
        output_dim=3,
    ),
    metadata={"sharpe": 1.85, "trained_on": "2026-06-01"},
)

# 3) 阶段流转：None -> Staging -> Production
registry.promote("ppo_btc", "1.0.0", stage=ModelStage.Staging)
# ... 跑几个小时的 shadow trading ...
registry.promote("ppo_btc", "1.0.0", stage=ModelStage.Production)

# 4) A/B：同时保留 production 和 challenger
registry.register_model("ppo_btc", "1.1.0", ...)        # 新版作 challenger
# 在 axon-inference 加载时按 stage 拉取
prod_path = registry.get_artifact_path("ppo_btc", stage=ModelStage.Production)
challenger_path = registry.get_artifact_path("ppo_btc", stage=ModelStage.Staging)

# 5) 回滚：把旧版本重新推到 production
registry.promote("ppo_btc", "0.9.0", stage=ModelStage.Production)

# 6) 列出所有版本
versions = registry.list_versions("ppo_btc", stage=ModelStage.Archived)
```

**Rust 侧（开发新 storage backend / 嵌入 CI 时使用）：**

```rust
use axon_registry::{ModelRegistry, ModelStage, LocalStorage};

let storage = LocalStorage::new("/var/axon/models")?;
let mut registry = ModelRegistry::new(Box::new(storage));
let version = registry.register_model("ppo_btc", "v1.0.0", model_bytes, signature)?;
registry.promote(&version, ModelStage::Staging)?;
```

### 关键依赖
- **依赖**：`axon-core`
- **被依赖**：`axon-inference`（加载生产模型）、`axon-tracker`（关联 run）、集成测试

---

## 10. `axon-llm`

### 核心职责
LLM 智能体：ReAct 推理循环 + Tool Calling + 上下文窗口 + 三个内置工具（市场 / 组合 / 下单）+ 多 Agent Swarm。

### 代码位置
- `crates/axon-llm/src/lib.rs` — 入口
- `crates/axon-llm/src/react_agent.rs` — `ReActAgent`（Reasoning + Acting）
- `crates/axon-llm/src/declarative_agent.rs` — 声明式 Agent（YAML 配置）
- `crates/axon-llm/src/context.rs` — `ContextManager`（滑动窗口 + 摘要压缩）
- `crates/axon-llm/src/prompt.rs` — `PromptTemplate`
- `crates/axon-llm/src/tools.rs` — `Tool` trait + 错误类型
- `crates/axon-llm/src/trading/` — 交易工具集（`PlaceOrderTool` / `QueryPortfolioTool` / `CancelOrderTool` / `ReplaceOrderTool`）+ `MockTradingBackend` / `PaperBackend` / `SafetyMode`
- `crates/axon-llm/src/swarm/` — 多 Agent 协作：`Orchestrator` / `MarketAgent` / `RiskAgent` / `AuditAgent` / `Vote` / `PaperTrading`
- `crates/axon-llm/src/backends/` — LLM 后端：`OpenAICompat` / `Mock` / `Recording` / `Retry` / `Cost` / `Streaming`
- `crates/axon-llm/src/explain/` — 决策解释桥接（与 `axon-explain` 集成）

### 核心机制
- **ReAct 循环**：`ReActAgent::run(input)` 循环：LLM 返回 Thought → Action → Observation → 再次 Thought → …，直到 `FinishReason`
- **工具调用**：`Tool::call(args) -> Result<Value>`，参数校验在工具内部
- **Swarm 投票**：`Vote { HardVote / SoftVote / WeightedVote }`，orchestrator 收集多 agent 决策后合并
- **后端重试**：`backends/retry.rs` 实现指数退避 + 熔断；`backends/cost.rs` 累计 token cost
- **录制回放**：`backends/recording.rs` 录制请求/响应用于 e2e 测试

### 适用场景
- 用 LLM 解读行情并下单（`ReActAgent` + `PlaceOrderTool`）
- 多 Agent 风控（`MarketAgent` 出信号，`RiskAgent` 校验，`AuditAgent` 留痕）
- LLM 提示词 A/B 测试（用 `backends::recording` 录制后离线对比）
- 通过 `MockTradingBackend` 在不接交易所的情况下跑端到端 demo

### 不适用场景
- 纳秒级延迟的自动交易（LLM 推理要 100ms+）
- 离线批量回测（`axon-backtest` + `axon-rl` 更合适）
- 生产大模型微调（这是推理 / 编排层，不是训练层）

### 怎么用

**Python 侧（主用法，3 种场景）：**

```python
from axon_quant.llm import (
    LLMConfig, make_backend, LLMMessage, LLMBackend, ReActAgent,
    PlaceOrderTool, QueryPortfolioTool, MockTradingBackend,
)
from axon_quant.llm.swarm import (
    SwarmOrchestrator, MarketAgent, RiskAgent, AuditAgent, VoteType,
)
import axon_quant

# ─── 场景 1：单 Agent + Tool Calling（最常见）───────────────────────
cfg = LLMConfig(
    backends=[{
        "name": "primary",
        "base_url": "https://api.openai.com/v1",
        "api_key": "sk-...",
        "model": "gpt-4o-mini",
        "temperature": 0.3,
        "max_tokens": 2048,
    }],
    retry={"max_retries": 5, "initial_backoff_ms": 100},
)
backend = make_backend(cfg)
resp = backend.chat([LLMMessage("user", "BTC 当前行情如何？")])
print(resp.content)

# ─── 场景 2：ReAct + 下单工具（接 axon-oms/axon-risk）──────────────
oms = axon_quant.oms.OrderManager()
oms.deposit("USDT", 100_000)

mock_backend = MockTradingBackend()
place_tool = PlaceOrderTool(backend=mock_backend, mode="dry_run",
                            risk={"max_order_notional": 100.0,
                                  "allowed_symbols": ["BTC-USDT"]})
agent = ReActAgent(backend=make_backend(cfg), tools=[place_tool])
result = agent.run("当前 BTC 是不是好的入场点？如果是就买入 0.1 BTC")

# ─── 场景 3：多 Agent Swarm（生产级多视角决策）──────────────────
orchestrator = SwarmOrchestrator(
    agents=[
        MarketAgent(backend=make_backend(cfg)),   # 出信号
        RiskAgent(backend=make_backend(cfg)),     # 风控校验
        AuditAgent(backend=make_backend(cfg)),    # 留痕
    ],
    vote_type=VoteType.SoftVote,                  # 软投票
)
decision = orchestrator.run({"symbol": "BTC-USDT", "market": bars[-100:]})
print(decision.action, decision.confidence, decision.votes)
```

**Rust 侧（开发新 Tool / 新后端时使用）：**

```rust
use axon_llm::{ReActAgent, MockBackend, PlaceOrderTool, QueryPortfolioTool};

let backend = MockBackend::new();
let agent = ReActAgent::builder(backend)
    .tool(Box::new(PlaceOrderTool::new(paper_backend)))
    .tool(Box::new(QueryPortfolioTool::new(portfolio)))
    .build();
let response = agent.run("当前 BTC 行情如何？是否应该加仓？").await?;
```

### 关键依赖
- **依赖**：`axon-core` / `axon-backtest`（回测工具）/ `axon-oms`（下单）/ `axon-explain`（解释，可选）
- **被依赖**：`axon-integration-tests`（e2e_react_loop_test / live_trading_e2e）

---

## 11. `axon-explain`

### 核心职责
可解释性：SHAP 特征归因 + 反事实解释 + 决策报告生成。

### 代码位置
- `crates/axon-explain/src/lib.rs` — 入口
- `crates/axon-explain/src/shap.rs` — KernelSHAP / TreeSHAP 实现
- `crates/axon-explain/src/counterfactual.rs` — 反事实解释（找最小改动能翻转决策的样本）
- `crates/axon-explain/src/report.rs` — 决策报告（HTML / JSON / Markdown）
- `crates/axon-explain/src/traits.rs` — `Explainer` / `CounterfactualSearch` trait
- `crates/axon-explain/src/python/` — PyO3 绑定

### 核心机制
- **KernelSHAP**：基于加权线性回归的 model-agnostic 解释器
- **反事实**：从当前样本出发，对特征做最小扰动直到模型输出翻转
- **报告**：`report::generate` 把 SHAP 值 + 反事实 + 原始输入组装成可读文档

### 适用场景
- 监管合规：解释为什么策略在某时刻下单（GDPR / MiFID II）
- 调试模型：哪个特征贡献最大、是否过拟合到噪声
- 决策评审：`axon-llm::swarm::AuditAgent` 可调用解释器生成解释
- 研究论文展示

### 不适用场景
- 训练时特征选择（用 L1 / Mutual Information 即可，SHAP 太慢）
- 实时决策（单样本 KernelSHAP 要几秒到几分钟）
- 黑盒外部 API（无法计算梯度时退而求其次，但只解释代理模型）

### 怎么用

**Python 侧（主用法，3 个核心场景）：**

```python
from axon_quant.explain import (
    KernelSHAP, CounterfactualConfig, ReportGenerator,
    ActionSnapshot, ActionAttribution, ContributionDirection,
)
import numpy as np

# ─── 场景 1：KernelSHAP 特征归因（最常见）───────────────────────
# 输入：模型 / background 数据 / 待解释样本
explainer = KernelSHAP(model=my_ppo_policy, background_data=X_train[:100])
attributions: ActionAttribution = explainer.explain(X_test[0])

# 每个特征的边际贡献
for feat, attr in zip(feature_names, attributions.values):
    direction = "+" if attr.direction == ContributionDirection.Positive else "-"
    print(f"  {direction} {feat}: {attr.marginal_contribution:+.3f}")

# ─── 场景 2：反事实解释（找最小扰动翻转决策）──────────────────
cf_config = CounterfactualConfig(
    target_class="Sell",            # 想翻成的目标动作
    max_features_perturbed=3,       # 最多改 3 个特征
    distance_metric="l1",
)
cf = explainer.counterfactual(
    instance=X_test[0],
    config=cf_config,
)
print("原始决策: Buy @ 50000")
print(f"最小改动 → {cf.target_class}: 把 {cf.changed_features} 改成 {cf.new_values}")
# e.g. 把 rsi_14 改成 75 → 决策翻为 Sell

# ─── 场景 3：决策报告（HTML / JSON / Markdown）──────────────────
gen = ReportGenerator(template="regulatory")     # 也支持 "minimal" / "full"
snapshot = ActionSnapshot(
    timestamp_ns=1_700_000_000_000_000_000,
    model_id="ppo_btc@1.0.0",
    input=X_test[0],
    output={"action": "Buy", "quantity": 0.1, "price": 50000.0},
    attributions=attributions,
    counterfactual=cf,
)
report = gen.generate(snapshot, format="html")
with open("/var/axon/reports/decision_20231115.html", "w") as f:
    f.write(report)
```

**Rust 侧（开发新 Explainer / 嵌入 LLM Agent 决策审计时使用）：**

```rust
use axon_explain::{KernelShap, Explainer};

let explainer = KernelShap::new(model, background_data)?;
let attributions = explainer.explain(&instance)?;
```

### 关键依赖
- **依赖**：`axon-core`
- **被依赖**：`axon-llm`（explain feature 桥接）、`axon-compliance`（生成合规报告）

---

## 12. `axon-ensemble`

### 核心职责
组合多个 RL / 规则策略，提高鲁棒性。提供投票 / 加权 / 堆叠三种策略。

### 代码位置
- `crates/axon-ensemble/src/lib.rs` — 入口
- `crates/axon-ensemble/src/manager.rs` — `EnsembleManager`（注册/卸载/调度子策略）
- `crates/axon-ensemble/src/voting.rs` — `HardVote` / `SoftVote` / `WeightedVote`
- `crates/axon-ensemble/src/stacking.rs` — `StackingEnsemble`（meta-model 二次学习）
- `crates/axon-ensemble/src/dynamic.rs` — `DynamicWeightedEnsemble`（按近期表现动态调权）
- `crates/axon-ensemble/src/traits.rs` — `Ensemble` / `Policy` / `VotingStrategy` trait
- `crates/axon-ensemble/src/types.rs` — `Action` / `ActionProbabilities` / `ModelPerformance`

### 核心机制
- **投票**：
  - `HardVote`：取多数票
  - `SoftVote`：概率平均
  - `WeightedVote`：按权重加权
- **堆叠**：以子模型输出为特征，训练一个 meta-model（logistic / 简单 MLP）
- **动态权重**：`DynamicWeightedEnsemble` 用近 N 步 reward 调权，表现好的子模型获得更大权重

### 适用场景
- 训练多组超参 / 不同 RL 算法后整合（典型提升 1-3% Sharpe）
- 跑 A/B 灰度上线（用 `DynamicWeightedEnsemble` 给新策略一个低权起点）
- 多时间框架策略融合（5min + 1h + 1d 三个子模型投票）
- `axon-integration-tests` 中 ensemble + walk-forward 的 e2e 例子

### 不适用场景
- 单策略基线（开销大于收益）
- 子模型同质化（5 个 PPO 同种子集成 ≈ 1 个 PPO）
- 推理延迟 < 1ms 的场景（每次决策要 N 次前向）

### 怎么用

**Python 侧（主用法，3 种集成策略）：**

```python
from axon_quant.ensemble import (
    EnsembleManager, EnsembleStrategy,
    HardVoteStrategy, SoftVoteStrategy, WeightedVoteStrategy,
    StackingEnsemble, MetaModel, Observation, ModelWeight,
)
import axon_quant

# ─── 1) 软投票（最常见）──────────────────────────────────
mgr = EnsembleManager(strategy=SoftVoteStrategy())
mgr.add_policy(axon_quant.inference.create_onnx_engine("ppo_v1.onnx", ...),
               weight=1.0)
mgr.add_policy(axon_quant.inference.create_onnx_engine("ppo_v2.onnx", ...),
               weight=1.0)
mgr.add_policy(axon_quant.inference.create_onnx_engine("sac_v1.onnx", ...),
               weight=0.7)

obs = Observation(features=current_state, symbol="BTC-USDT")
action = mgr.decide(obs)
print(action.action_type, action.confidence)

# ─── 2) 硬投票（多数决定）─────────────────────────────────
mgr = EnsembleManager(strategy=HardVoteStrategy())

# ─── 3) 加权投票（每个模型独立权重）────────────────────────
mgr = EnsembleManager(
    strategy=WeightedVoteStrategy(weights={
        "ppo_v1": 1.0, "ppo_v2": 0.8, "sac_v1": 0.5,
    })
)

# ─── 4) 堆叠：训练一个 meta-model 二次学习─────────────────────
stack = StackingEnsemble(
    meta_model=MetaModel.MLP,
    n_folds=5,                  # K-fold 交叉验证防泄漏
)
stack.add_base_model("ppo_v1", axon_quant.inference.create_onnx_engine("ppo_v1.onnx", ...))
stack.add_base_model("sac_v1", axon_quant.inference.create_onnx_engine("sac_v1.onnx", ...))
stack.fit(X_meta_train, y_meta_train)  # 用 OOS 预测作为 meta 特征
action = stack.predict(obs)

# ─── 5) 动态权重：按近期表现自动调权────────────────────────
# 表现好的子模型获得更大权重（reward-weighted）
mgr.update_performance("ppo_v1", recent_reward=2.5)
mgr.update_performance("ppo_v2", recent_reward=1.8)
```

**Rust 侧（开发新投票策略 / 嵌入训练时使用）：**

```rust
use axon_ensemble::{EnsembleManager, WeightedVoteStrategy, Policy};

let mut mgr = EnsembleManager::new(Box::new(WeightedVoteStrategy::new()));
mgr.add_policy(Box::new(ppo_policy_a));
mgr.add_policy(Box::new(ppo_policy_b));
let action = mgr.decide(&observation);
```

### 关键依赖
- **依赖**：`axon-core`
- **被依赖**：`axon-rl`（多策略训练）、`axon-llm`（多 Agent 决策近似）

---

## 13. `axon-data`

### 核心职责
市场数据统一接入：Mock / CSV / Parquet / 缓存（mmap）+ 特征管道（归一化 / 滑窗）。

### 代码位置
- `crates/axon-data/src/lib.rs` — 入口
- `crates/axon-data/src/sources/mock.rs` — `MockSource`（默认开启）
- `crates/axon-data/src/sources/csv.rs` — `CsvSource`（`csv-source` feature）
- `crates/axon-data/src/sources/parquet.rs` — `ParquetSource`（`parquet-source` feature）
- `crates/axon-data/src/cache/control.rs` — L1 缓存（`CacheControl`）
- `crates/axon-data/src/cache/mmap.rs` — L2 mmap 共享缓存（`mmap-cache` feature）
- `crates/axon-data/src/cache/shared_memory.rs` — 跨进程共享内存
- `crates/axon-data/src/pipeline.rs` — `FeaturePipeline`（`ZScoreNormalizer` / `FeatureMatrix`）
- `crates/axon-data/src/dataset.rs` — 行式 `Dataset` 抽象
- `crates/axon-data/src/bar/` — K 线聚合
- `crates/axon-data/src/ipc/` — Arrow IPC
- `crates/axon-data/src/python/` — PyO3 绑定（`axon_quant.data`）

### 核心机制
- **统一 trait**：`DataSource::fetch(&DataRequest) -> Result<Dataset>`
- **L1 缓存**：`CacheControl` 维护 `HashMap<key, Arc<Dataset>>` + LRU
- **L2 mmap**：`mmap-cache` feature 用 `memmap2` 把 Parquet mmap 进来，多进程共享零拷贝
- **特征管道**：`FeaturePipeline` 链式组合 `ZScoreNormalizer` / `MinMax` / `Robust`，生成训练用 `FeatureMatrix`
- **Bar 聚合**：`bar::BarDataset` 把 Tick 聚合成 1m / 5m / 1h K 线

### 适用场景
- 接历史 CSV / Parquet 做回测
- 跑 HPO 时共享 L2 mmap 缓存（多 trial 并行读同一份数据零拷贝）
- 用 `MockSource` 写单元测试
- 把原始 Tick 聚合成 K 线后灌给 RL 环境

### 不适用场景
- 实时行情接入（那是 `axon-exchange` 的领域）
- 复杂的因子计算（应使用专门的 feature store）
- 跨机器缓存（当前 mmap 是单机内）

### 怎么用

**Python 侧（主用法，4 类数据源 + 缓存）：**

```python
from axon_quant.data import (
    DataService, DataRequest, Frequency, MockSource, CsvSource, ParquetSource,
    CacheControl, AxonError, DataError,
)
import datetime
import pyarrow as pa

# ─── 1) Mock 数据（单元测试首选）─────────────────────────────
svc = DataService.new().register_source(
    MockSource.with_tick_series("btc", 1000, 1_000_000, lambda i: 100.0 + i)
)
req = DataRequest("BTCUSDT", "2026-01-01T00:00:00Z",
                  "2026-01-02T00:00:00Z", Frequency.Min1)
ds = svc.load(req)
print(ds.len)        # 1000
batch = ds.to_arrow(0)  # pyarrow.RecordBatch（零拷贝）

# ─── 2) CSV / Parquet（生产数据接入）─────────────────────────
svc = (DataService.new()
       .register_source(CsvSource(root_dir="/data/csv", tz="UTC"))
       .register_source(ParquetSource(root_dir="/data/parquet")))

req = DataRequest("BTCUSDT",
                  datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc),
                  datetime.datetime(2026, 6, 1, tzinfo=datetime.timezone.utc),
                  Frequency.Min1)
ds = svc.load(req)

# ─── 3) L1 缓存（默认开启，跨请求复用）──────────────────────
cache = CacheControl(max_entries=128, ttl_seconds=600)
svc = DataService.new().with_cache(cache).register_source(...)

# ─── 4) 配合回测 / RL env（最常见消费路径）────────────────
from axon_quant.backtest import BacktestEngine
from axon_quant.rl import TradingEnv

# 用 DataService 拿数据 → 转 numpy 给回测
bars_array = ds.to_numpy()  # shape: (n_bars, n_features)
env = TradingEnv(config={"initial_capital": 100_000},
                 market_data=bars_array, reward="sharpe")
```

**Cargo feature 启用（需要 CSV / Parquet / mmap 时）：**

```toml
[dependencies]
axon-data = { path = "../axon-data",
              features = ["csv-source", "parquet-source", "mmap-cache"] }
```

**Rust 侧（自定义 DataSource 实现时使用）：**

```rust
use axon_data::{MockSource, DataSource, DataRequest, Frequency};

let src = MockSource::new();
let dataset = src.fetch(&DataRequest::bars("BTC-USDT", Frequency::Min1, 1000))?;
```

### 关键依赖
- **依赖**：`axon-core`、arrow / parquet
- **被依赖**：`axon-backtest`（历史数据回放）、`axon-rl`（观测数据源）、`axon-inference`（批量推理数据）

---

## 14. `axon-compliance`

### 核心职责
金融交易合规审计：交易记录 + 区块链式审计日志（日志哈希链，防篡改）+ 报告生成 + 监管报送。

### 代码位置
- `crates/axon-compliance/src/lib.rs` — `ComplianceModule` 主结构
- `crates/axon-compliance/src/audit/log.rs` — `AuditLog`（哈希链追加）
- `crates/axon-compliance/src/audit/storage.rs` — `FileStorage`（按日期分文件持久化）
- `crates/axon-compliance/src/regulator/metrics.rs` — 监管指标计算（集中度 / 大额 / 持仓限额）
- `crates/axon-compliance/src/regulator/submission.rs` — 监管报送生成 + 导出
- `crates/axon-compliance/src/report/daily.rs` / `monthly.rs` / `annual.rs` / `formatter.rs` — 日 / 月 / 年报
- `crates/axon-compliance/src/types.rs` — `TradeRecord` / `TradeStatus` / `AuditEvent` / `ComplianceConfig`
- `crates/axon-compliance/src/python/` — PyO3 绑定

### 核心机制
- **哈希链审计**：`AuditLog` 中每条 `AuditEvent` 的 `event_hash = sha256(prev_hash || event_payload)`，验证时重算整链
- **大额告警**：`TradeRecord.notional_value > large_trade_threshold` 时记录 `tracing::warn!` 但**不阻止**交易
- **报告生成器**：`daily::DailyReportGenerator` / `monthly::MonthlyReportGenerator` / `annual::AnnualReportGenerator` 各自实现固定字段
- **导出**：`ReportExporter::export(report, format)` 支持 JSON / CSV / 自定义格式

### 适用场景
- 真实账户的合规留痕（MiFID II / SEC / 中国证监会要求 7 年留存）
- 内部审计：能证明 "这个决策由 X 策略在 Y 时间生成"
- 监管报送：每月生成 `RegulatorySubmission` 上报到指定 regulator
- 单元测试中验证哈希链完整性

### 不适用场景
- 实时风险阻断（`large_trade_threshold` 仅告警不阻止，要阻止应使用 `axon-risk`）
- 交易执行路径（应在 `axon-oms` 之后异步调用，不应阻塞下单）
- 大量历史数据查询（应使用专门的 OLAP 存储）

### 怎么用

**Python 侧（主用法，从配置到报表全链路）：**

```python
from axon_quant.compliance import (
    ComplianceModule, ComplianceConfig, load_config_from_toml,
    TradeSide, OrderType, LiquidityType, TradeStatus,
    ComplianceError,
)
from decimal import Decimal

# 1) 构造配置（推荐从 TOML 读）
cfg = ComplianceConfig(
    account_id="ACC-001",
    base_currency="USDT",
    large_trade_threshold=100_000.0,           # 大额告警阈值
    position_limit=1_000_000.0,                # 单标的上限
    max_portfolio_concentration=0.4,           # 最大集中度
    data_retention_years=7,                    # MiFID II / CSRC 要求
    regulators=["SEC", "CSRC"],
)
# 或从文件读: cfg = load_config_from_toml("compliance.toml")

# 2) 启动模块（内部用 Blake3/SHA256 哈希链持久化到 audit_dir）
cm = ComplianceModule(cfg, audit_dir="/var/axon/audit")

# 3) 记录一笔成交
cm.record_trade({
    "trade_id": "T-2026-0715-0001",
    "strategy_id": "ppo_btc@1.0.0",
    "symbol": "BTCUSDT",
    "side": TradeSide.Buy,
    "order_type": OrderType.Limit,
    "liquidity": LiquidityType.Taker,
    "quantity": Decimal("0.1"),
    "price": Decimal("50000.0"),
    "notional_value": Decimal("5000.0"),
    "fee": Decimal("5.0"),
    "status": TradeStatus.Filled,
    "venue": "binance",
    "executed_at_ns": 1_700_000_000_000_000_000,
})

# 4) 验证哈希链完整性（崩溃恢复后必跑）
assert cm.verify_audit_integrity(), "审计链被篡改！"

# 5) 生成日 / 月 / 年报
daily = cm.generate_report(period="daily", date="2026-07-15")
cm.export_report(daily, format="json", path="/var/axon/reports/daily_0715.json")

# 6) 生成监管报送文件
submission = cm.generate_regulatory_submission(regulator="SEC", period="monthly")
cm.export_report(submission, format="csv", path="/var/axon/submissions/sec_2026-07.csv")

# 7) 大额交易实时告警（订阅）
def on_large_trade(trade):
    print(f"⚠️ 大额交易: {trade.symbol} {trade.notional_value} {trade.side}")
cm.subscribe_large_trade(threshold=50_000.0, callback=on_large_trade)
```

**Rust 侧（开发新报告模板 / 嵌入 oms 异步落审计时使用）：**

```rust
use axon_compliance::{ComplianceModule, ComplianceConfig, TradeRecord, TradeSide, TradeStatus, OrderType, LiquidityType};

let cfg = ComplianceConfig {
    account_id: "test".into(),
    base_currency: "USDT".into(),
    large_trade_threshold: 100_000.0,
    position_limit: 1_000_000.0,
    max_portfolio_concentration: 0.4,
    data_retention_years: 7,
    regulators: vec!["SEC".into()],
};
let mut cm = ComplianceModule::new(cfg, "/tmp/audit")?;
cm.record_trade(TradeRecord { /* ... */ })?;
assert!(cm.verify_audit_integrity());
```

### 关键依赖
- **依赖**：`axon-core`、`chrono` / `uuid` / `sha2`
- **被依赖**：`axon-oms`（异步落审计）、`axon-risk`（联动告警）

---

## 15. `axon-risk`

### 核心职责
**预交易**风控：订单大小 / 仓位 / 杠杆 / 回撤检查 + 熔断器（连续亏损自动暂停）+ 组合监控 + VaR。

### 代码位置
- `crates/axon-risk/src/lib.rs` — 入口
- `crates/axon-risk/src/engine.rs` — `DefaultRiskEngine`（`RiskEngine` trait 实现）
- `crates/axon-risk/src/checks.rs` — 各种 check 函数
- `crates/axon-risk/src/circuit_breaker.rs` — `CircuitBreaker`（AtomicU8 状态机）
- `crates/axon-risk/src/config.rs` — `RiskConfig`（阈值 / 窗口 / 限制）
- `crates/axon-risk/src/metrics.rs` — `RiskMetrics`（实时指标）
- `crates/axon-risk/src/handler.rs` — `RiskEventHandler`（事件驱动）
- `crates/axon-risk/src/python/` — PyO3 绑定

### 核心机制
- **检查链**（典型 12ns 总开销）：
  - 熔断器（AtomicBool，~5ns，不活跃直接返回）
  - 订单大小（~10ns）
  - 仓位限制（~50ns，HashMap 查找）
  - 杠杆（~20ns）
  - 回撤（~20ns）
- **熔断器**：连续 N 次亏损 → 状态 `Closed → Open`，冷却期后 `HalfOpen` 试探，成功则 `Closed`
- **VaR**：历史模拟法，用最近 N 日收益分布估算 95% / 99% 分位损失

### 适用场景
- 实盘下单前的强制门禁（`axon-oms` 在 submit 前调 `engine.check_order`）
- 监控组合回撤，触发熔断后暂停策略
- 计算实时的 VaR / 杠杆率 / 集中度用于风控面板
- `axon-harness::HarnessBridge` 也用本模块做策略级门控

### 不适用场景
- 回测阶段（回测本身就是历史数据，强制风控会失真；如要模拟用 `ImpactedMatchingEngine`）
- 低延迟高频（12ns 是无锁设计的极限，再低要 FPGA）
- 复杂的合规留痕（那是 `axon-compliance`）

### 怎么用

**Python 侧（主用法，预交易门禁 + 熔断）：**

```python
from axon_quant.risk import (
    DefaultRiskEngine, RiskConfig, CircuitBreaker,
    RiskResult, RiskReason, RiskMetrics, RiskError,
    make_order, make_portfolio, make_portfolio_with_positions,
    make_risk_config,
)
import axon_quant

# 1) 构造风控配置
cfg = make_risk_config(
    max_order_value=10_000.0,           # 单笔最大名义价值
    max_position_per_symbol=100.0,      # 单标的最大持仓
    max_total_exposure=1_000_000.0,     # 总敞口上限
    max_leverage=3.0,                   # 杠杆上限
    max_drawdown=0.20,                  # 最大回撤 20%
    max_daily_loss=5_000.0,             # 日内亏损熔断阈值
    max_concentration=0.4,              # 单标的占比上限
    circuit_breaker_cooldown_s=300,     # 熔断冷却 5 分钟
)
engine = DefaultRiskEngine(cfg)

# 2) 预交易门禁（每个订单提交前必过）
order = make_order(symbol="BTC-USDT", side="Buy", type="limit",
                   price=50_000.0, quantity=0.1)
portfolio = make_portfolio(base_currency="USDT",
                           cash={"USDT": 100_000.0})
result = engine.check_order(order, portfolio)
if result.is_allow:
    axon_quant.oms.OrderManager().submit(order)
elif result.is_reject:
    log.warning(f"风控拒绝: {result.reason}")  # RiskReason 枚举

# 3) 熔断器
breaker = CircuitBreaker(
    max_consecutive_losses=5,           # 连续 5 笔亏就熔断
    cooldown_seconds=300,
)
if breaker.check_and_trigger(recent_pnl=-200.0):
    # ... 暂停策略,等冷却
    pass

# 4) 累计日内 PnL 触发熔断
engine.update_daily_pnl(-1_500.0)
# 下一笔订单会因日内亏损超阈值被自动拒绝
assert not engine.check_order(order, portfolio).is_allow
engine.reset_daily()  # 每天 0 点重置

# 5) 实时风险指标
m: RiskMetrics = engine.metrics(portfolio)
print(f"NAV={m.nav}  leverage={m.leverage:.2f}  "
      f"drawdown={m.drawdown:.2%}  VaR95={m.var_95:.2f}")
```

**Rust 侧（开发新 check / 嵌入 oms 预检时使用）：**

```rust
use axon_risk::{DefaultRiskEngine, RiskEngine, RiskResult};

let engine = DefaultRiskEngine::new(RiskConfig::default());
match engine.check_order(&order, &portfolio) {
    RiskResult::Allow => oms.submit(order)?,
    RiskResult::Reject(reason) => log::warn!("rejected: {:?}", reason),
    RiskResult::Warn(msg) => { oms.submit(order)?; log::warn!("warning: {}", msg); }
}
```

### 关键依赖
- **依赖**：`axon-core` / `axon-oms`（订单类型）
- **被依赖**：`axon-exchange`（实盘下单门禁）、`axon-oms`（预检）、`axon-harness`

---

## 16. `axon-inference`

### 核心职责
推理引擎：ONNX / tch / Candle 三后端 + CPU/CUDA/Metal 多设备 + 批推理管线 + 模型热更新 + **CPU 亲和性绑定**。

### 代码位置
- `crates/axon-inference/src/lib.rs` — 入口
- `crates/axon-inference/src/engine.rs` — `InferenceEngine` 单模型推理
- `crates/axon-inference/src/backend/candle.rs` — Candle 后端（纯 Rust）
- `crates/axon-inference/src/backend/onnx.rs` — ONNX Runtime 后端
- `crates/axon-inference/src/backend/tch.rs` — tch-rs（PyTorch C++）后端
- `crates/axon-inference/src/pipeline/batch.rs` — `BatchInferencePipeline`（tokio + rayon）
- `crates/axon-inference/src/pipeline/collector.rs` — 请求收集器
- `crates/axon-inference/src/hot_reload.rs` — `ModelHotReloader`（notify 文件监控）
- `crates/axon-inference/src/affinity.rs` — **CPU/GPU 线程亲和性**（独立子模块）
- `crates/axon-inference/src/python/` — PyO3 绑定

### 核心机制
- **后端 trait**：`Backend::load / infer / warmup`
- **批推理**：`BatchInferencePipeline::submit(obs)` 推入有界 channel，collector 在窗口内聚批，触发 `infer_batch`
- **热更新**：`notify` 监听 `model_path` 变化 → 加载新模型 → 原子替换 `ArcSwap`
- **CPU 亲和性**（举例说明）：
  - **核心价值**：保证批推理管线的 P99 延迟稳定，减少多模型并发时的 cache thrashing
  - **平台支持**：Linux（`sched_setaffinity`）+ macOS（`thread_policy_set`，MPS-aware）；Windows 运行时拒绝（用户用 WSL2 / numactl）
  - **GPU 亲和性**：CUDA 用 `with_cuda(device_id)` 触发 `cudaSetDevice`；Metal 仅做 MPS 检查（Metal 没有 thread-level set device API）
  - **使用场景**：
    - ✅ 用 `BatchInferencePipeline` 跑 RL 策略时**自动生效**（`axon-rl` 的 PPO 推理）
    - ✅ 多模型并发服务（不同模型绑不同 core）
    - ❌ 回测撮合引擎（单线程确定性设计天然不需要）
    - ❌ LLM Agent 单次推理（请求是低频的，绑核反而浪费）

### 适用场景
- 训练好的 RL 策略转 ONNX 后做低延迟推理
- 多模型 A/B（`BatchInferencePipeline` 同时跑 2 个模型）
- 模型在生产中持续更新（`ModelHotReloader` 监听文件）
- 任何需要稳定 P99 延迟的服务

### 不适用场景
- 模型训练（这是推理引擎，不反向传播）
- 大模型 LLM 推理（应使用 vLLM / TGI，本模块是给小模型设计的）
- 简单脚本里临时调一次（加载开销大于收益）

### 怎么用

**Python 侧（主用法，4 类推理场景）：**

```python
from axon_quant.inference import (
    InferenceEngine, ModelConfig, Device, Observation, Action,
    InferenceBackend, BatchInferencePipeline, ModelHotReloader,
    create_onnx_engine, create_candle_engine, create_inference_engine,
    pin_current_thread_to_cpus, get_affinity_plan, InferenceError,
)
import os

# ─── 1) 一步创建 + 加载（最常见）────────────────────────────
engine = create_onnx_engine(
    model_path="model.onnx",
    input_shape=(1, 64, 128),
    output_dim=3,
    device=Device.Cpu,
    num_threads=4,
)
obs = Observation(symbol="BTC-USDT", timestamp_ns=1_000_000_000,
                  features=[0.0] * 128)
action: Action = engine.infer(obs)
print(action.action_type, action.confidence)

# ─── 2) Candle 后端（纯 Rust,无 ONNX Runtime 依赖）──────────
engine = create_candle_engine(
    model_path="model.safetensors",
    input_shape=(1, 64, 128),
    output_dim=3,
)

# ─── 3) 批推理管线（高 QPS 服务）──────────────────────────
pipeline = BatchInferencePipeline(
    model_config=ModelConfig(
        path="model.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.Cpu,
        input_shape=(1, 64, 128),
        output_dim=3,
    ),
    batch_window_us=2_000,          # 2ms 窗口
    max_batch_size=64,
)
action = pipeline.submit(obs)
# P99 延迟 < 5ms,单核 1 万 QPS

# ─── 4) 模型热更新（生产中持续迭代）────────────────────────
reloader = ModelHotReloader(watch_path="/var/axon/models/ppo_btc/current.onnx")
reloader.on_reload(lambda path: pipeline.update_model(path))
reloader.start()                    # 监听文件变化

# ─── 5) CPU 亲和性绑定（生产级低延迟）──────────────────────
# 注意:自动生效需要满足前提
# - ✅ BatchInferencePipeline + 多模型并发 (推荐)
# - ❌ BacktestEngine 撮合(单线程不需要)
# - ❌ LLM Agent 单次推理(请求低频,绑核浪费)
plan = get_affinity_plan(worker_count=4)
pin_current_thread_to_cpus([0, 1, 2, 3])   # 当前线程绑核 0-3

# 平台支持:Linux/macOS ✅  Windows ❌ 运行时拒绝,需用 WSL2/numactl
```

**Rust 侧（开发新后端 / 嵌入 RL 训练时使用）：**

```rust
use axon_inference::{BatchInferencePipeline, ModelConfig, InferenceBackend, Device, Observation};
use axon_inference::affinity::{pin_current_thread_to_cpus, AffinityPlan};

pin_current_thread_to_cpus(&[0, 1, 2, 3])?;

let cfg = ModelConfig {
    path: "model.onnx".into(),
    backend: InferenceBackend::Onnx,
    device: Device::Cpu,
    input_shape: [1, 64, 128],
    output_dim: 3,
    fp16: false,
    num_threads: 4,
};
let pipeline = BatchInferencePipeline::new(cfg, 2_000_000, 64)?;
let action = pipeline.submit(Observation { /* ... */ }).await?;
```

### 关键依赖
- **依赖**：`axon-core`、ort / tch / candle（按 feature）
- **被依赖**：`axon-rl`（策略推理）、`axon-llm`（LLM 后端可选）、`axon-registry`（加载生产模型）

---

## 17. `axon-exchange`

### 核心职责
交易所对接：Binance / OKX 的 REST + WebSocket 适配器 + 指数退避重连 + 令牌桶限流 + 订单生命周期管理。

### 代码位置
- `crates/axon-exchange/src/lib.rs` — 入口 + `build_http_client`
- `crates/axon-exchange/src/traits.rs` — `ExchangeAdapter` trait
- `crates/axon-exchange/src/adapters/binance.rs` — Binance USDⓈ-M 合约适配器
- `crates/axon-exchange/src/adapters/okx.rs` — OKX V5 适配器
- `crates/axon-exchange/src/ws/manager.rs` — `WebSocketManager`（自动重连 + 熔断）
- `crates/axon-exchange/src/ws/protocol.rs` — 协议编解码
- `crates/axon-exchange/src/sign/binance.rs` / `sign/okx.rs` — 签名
- `crates/axon-exchange/src/rate_limiter.rs` — `TokenBucketRateLimiter`
- `crates/axon-exchange/src/lifecycle.rs` — `OrderLifecycleManager`（本地状态机）
- `crates/axon-exchange/src/python/` — PyO3 绑定

### 核心机制
- **指数退避**：`ReconnectConfig { initial_backoff, max_backoff, backoff_multiplier }`，连续失败次数翻倍
- **限流**：`TokenBucketRateLimiter { requests_per_second, orders_per_minute, ws_messages_per_second }` 各自独立
- **签名**：`sign::binance::sign(query, secret)` → HMAC-SHA256；OKX 用 HMAC-SHA256 + Base64
- **生命周期**：`OrderRecord { id, exchange_id, status, created_at, updated_at }`，`OrderLifecycleManager` 维护本地状态

### 适用场景
- 实盘对接 Binance / OKX 合约（用 `BinanceAdapter` / `OkxAdapter`）
- 跑测试网验证策略（`ExchangeConfig.testnet = true`）
- 配合 `axon-risk` 做风控门禁 + `axon-monitor` 做延迟监控
- 订单状态本地缓存 + 崩溃恢复（`OrderLifecycleManager`）

### 不适用场景
- 美股 / A 股（暂不支持）
- 现货（当前是合约适配器）
- 跨交易所套利（需要在多个 adapter 间快速切换，单实例做不到）

### 怎么用

**Python 侧（主用法，Binance / OKX 接入）：**

```python
import os
from axon_quant.exchange import (
    BinanceAdapter, OkxAdapter, ExchangeId,
    binance_testnet_config, okx_testnet_config,
    OrderLifecycleManager, TokenBucketRateLimiter,
    ExchangeError,
)

# ─── 1) Binance 合约：testnet 默认,key 从环境变量读 ─────────
os.environ["BINANCE_API_KEY"] = "your_key"
os.environ["BINANCE_API_SECRET"] = "your_secret"

cfg = binance_testnet_config()                # 默认 testnet=True
adapter = BinanceAdapter(cfg)
adapter.connect()

# 2) 下单(同步阻塞,Rust 端已 block_on tokio)
order_id = adapter.place_order({
    "symbol": "BTCUSDT",
    "side": "buy",
    "type": "limit",
    "quantity": "0.1",
    "price": "50000",
    "tif": "GTC",
})
print(f"已下单: {order_id}")

# 3) 撤单 / 改单
adapter.cancel_order(order_id)
adapter.replace_order(order_id, new_price="50100", new_quantity="0.1")

# 4) 查询订单状态 / 历史
status = adapter.get_order_status(order_id)
open_orders = adapter.get_open_orders("BTCUSDT")

# 5) 订单生命周期本地管理(崩溃恢复)
mgr = OrderLifecycleManager()
cid = mgr.register_order({
    "symbol": "BTCUSDT", "side": "buy", "type": "limit",
    "quantity": "0.1", "price": "50000", "tif": "GTC",
    "exchange": "binance",
})
mgr.update_status(cid, {"status": "filled", "filled_qty": "0.1", "avg_price": "50000"})
print(mgr.active_count(), mgr.history_count())

# 6) 限流保护(防止触发交易所 API 限制)
limiter = TokenBucketRateLimiter(
    requests_per_second=10,         # REST 限制
    orders_per_minute=1200,         # 下单限制
    ws_messages_per_second=5,       # WS 心跳限制
)
# adapter 内部自动用 limiter

adapter.disconnect()

# ─── OKX 类似 ────────────────────────────────────
os.environ["OKX_API_KEY"] = "..."
os.environ["OKX_API_SECRET"] = "..."
os.environ["OKX_PASSPHRASE"] = "..."    # OKX 特有

okx = OkxAdapter(okx_testnet_config())
okx.connect()
okx.place_order({"symbol": "BTC-USDT-SWAP", "side": "buy", ...})
okx.disconnect()
```

**Rust 侧（开发新交易所适配器 / 嵌入 low-level 调度时使用）：**

```rust
use axon_exchange::{BinanceAdapter, ExchangeConfig, ExchangeId, RateLimitConfig, ReconnectConfig};

let cfg = ExchangeConfig {
    exchange_id: ExchangeId::Binance,
    api_key: std::env::var("BINANCE_KEY")?,
    api_secret: std::env::var("BINANCE_SECRET")?,
    testnet: true,
    rest_base_url: "https://testnet.binance.vision".into(),
    ws_url: "wss://testnet.binance.vision/ws".into(),
    rate_limit: RateLimitConfig::default(),
    reconnect: ReconnectConfig::default(),
};
let adapter = BinanceAdapter::new(cfg)?;
let order_id = adapter.place_limit("BTCUSDT", "BUY", 0.01, 50_000.0).await?;
```

### 关键依赖
- **依赖**：`axon-core` / `axon-oms` / `reqwest` / `tokio-tungstenite`
- **被依赖**：`axon-llm::trading`（实盘 trading agent）、生产部署 pipeline

---

## 18. `axon-oms`

### 核心职责
订单管理系统：状态机（New → Submitted → Acknowledged → PartiallyFilled → Filled/Cancelled/Rejected）+ 幂等性 + 快照/恢复 + 批量操作。

### 代码位置
- `crates/axon-oms/src/lib.rs` — 入口
- `crates/axon-oms/src/manager.rs` — `OrderManager`（核心）
- `crates/axon-oms/src/portfolio.rs` — `Portfolio` / `Position` / `PortfolioSnapshot`
- `crates/axon-oms/src/types.rs` — `Order` / `OrderStatus` / `Side` / `TimeInForce`
- `crates/axon-oms/src/error.rs` — `OmsError`
- `crates/axon-oms/src/python/` — PyO3 绑定

### 核心机制
- **幂等性**：`Order` 含 `idempotency_key`，相同 key 二次 submit 直接返回原 id
- **状态机**：`OrderStatus` 转移在 `manager.rs` 中以 `matches!` 守卫，非法转移返回 `OmsError::InvalidTransition`
- **快照**：`OrderManager::snapshot() -> Vec<u8>`（4.9µs / 100 订单），崩溃后可 `restore(bytes)`
- **批量**：`batch_submit` / `batch_cancel` 在同一锁内完成，避免竞态

### 适用场景
- 实盘 OMS（接 `axon-exchange`）
- RL 训练中模拟订单簿状态（与 `axon-backtest` 配合）
- 持久化订单（snapshot → 存盘 → 启动时 restore）
- 幂等保护：网络重试时不会重复下单

### 不适用场景
- 历史回放（那是 `axon-backtest` 的领域）
- 撮合本身（`axon-oms` 只管理订单生命周期，不撮合）
- 跨账户管理（OMS 是单账户的，多账户应上层封装）

### 怎么用

**Python 侧（主用法，完整生命周期管理）：**

```python
from axon_quant.oms import (
    OrderManager, Order, OrderStatus, Side, OrderType,
    Portfolio, Position, OmsError,
    limit_order, market_order, make_order_status,
)
from decimal import Decimal

# 1) 启动 OMS + 初始资金
oms = OrderManager()
oms.deposit("USDT", 100_000)

# 2) 提交订单(工厂函数自动处理 Decimal 精度)
oid = oms.submit(limit_order(
    symbol="BTC-USDT", side="Buy",
    quantity=0.1, price=50_000,
    idempotency_key="ppo-btc-20260715-001",  # 幂等性 key
))
print(f"订单 id: {oid}")

# 3) 状态机推进
oms.update_status(oid, make_order_status("Acknowledged"))
oms.update_status(oid, make_order_status("Submitted"))

# 4) 处理部分成交/完全成交
oms.add_fill(
    order_id=oid,
    fill_id="f-001",
    symbol="BTC-USDT",
    price=50_000,
    quantity=0.05,          # 正=buy,负=sell
    fee=5.0,
)
oms.update_status(oid, make_order_status("PartiallyFilled",
                                         filled_qty=0.05, avg_price=50_000))

oms.add_fill(order_id=oid, fill_id="f-002",
             symbol="BTC-USDT", price=50_010, quantity=0.05, fee=5.0)
oms.update_status(oid, make_order_status("Filled",
                                         filled_qty=0.1, avg_price=50_005))

# 5) 撤单 / 改单
oms.cancel(oid)
# oms.update_status(oid, make_order_status("Rejected", reason="insufficient"))

# 6) 幂等性:相同 key 二次 submit 返回原 id,不重复下单
oid2 = oms.submit(limit_order(
    symbol="BTC-USDT", side="Buy",
    quantity=0.1, price=50_000,
    idempotency_key="ppo-btc-20260715-001",  # 同 key
))
assert oid == oid2

# 7) 快照(4.9µs / 100 订单) + 崩溃恢复
snap_bytes = oms.snapshot()                     # Vec<u8>
with open("/var/axon/oms/snap.bin", "wb") as f:
    f.write(snap_bytes)
# 重启后:
# oms.restore(snap_bytes)

# 8) 查询 portfolio
bal = oms.snapshot_balance()
print(bal["cash"]["USDT"])                       # 剩余现金
for pos in bal["positions"]:
    print(pos["symbol"], pos["quantity"], pos["avg_price"], pos["realized_pnl"])

print(f"active={oms.active_count()} history={oms.history_count()}")
```

**Rust 侧（开发新 Portfolio 算法 / 嵌入 low-level 调度时使用）：**

```rust
use axon_oms::{OrderManager, Order, OrderStatus, Side, OrderType};
use rust_decimal::Decimal;

let oms = OrderManager::new();
let order = Order::new("BTC-USDT".into(), Side::Buy, OrderType::Limit,
    Decimal::new(1, 3), Decimal::from(50_000));
let id = oms.submit(order)?;
oms.update_status(id, OrderStatus::Acknowledged)?;
```

### 关键依赖
- **依赖**：`axon-core` / `rust_decimal`
- **被依赖**：`axon-exchange`（实盘 submit）、`axon-llm::trading`（LLM 下单工具）、`axon-risk`（预检）

---

## 19. `axon-monitor`

### 核心职责
生产监控：原子指标（Counter / Gauge / Histogram）+ 告警规则 + 健康检查 + Prometheus 导出。

### 代码位置
- `crates/axon-monitor/src/lib.rs` — 入口
- `crates/axon-monitor/src/metrics.rs` — `AtomicCounter` / `AtomicGauge` / `LatencyHistogram` / `LatencyPercentiles`
- `crates/axon-monitor/src/registry.rs` — `MetricsRegistry`
- `crates/axon-monitor/src/alert.rs` — `AlertRule` / `AlertEvent` / `ThresholdCondition`
- `crates/axon-monitor/src/health.rs` — `HealthService` / `ComponentHealth`
- `crates/axon-monitor/src/error.rs` — `MonitorError`

### 核心机制
- **原子指标**：`AtomicCounter` 用 `AtomicU64::fetch_add`（1.6ns），`AtomicGauge` 用 `AtomicU64` 存 bits（464ps）
- **直方图**：`LatencyHistogram` 用固定桶（ns/µs/ms/s）+ 原子计数
- **告警**：注册 `AlertRule::Threshold { metric, condition, severity, message }`，`check_alerts(name, value)` 触发
- **健康检查**：`HealthService` 收集各组件 `HealthCheck::check() -> ComponentHealth`

### 适用场景
- 实盘服务暴露 Prometheus 指标（`/metrics` 端点）
- 订单延迟 P99 超过阈值时告警（接 Slack / PagerDuty）
- Kubernetes liveness / readiness 探针（用 `HealthService`）
- 性能基线（每纳秒都有数据，可绘制火焰图关联）

### 不适用场景
- 业务语义指标（这是低层计数器，不是 BI）
- 长期存储（应 push 到 Prometheus + 远端 TSDB）
- 复杂告警路由（用 Alertmanager）

### 怎么用

**Python 侧（主用法，3 类用途）：**

```python
from axon_quant.monitor import (
    MetricsRegistry, AtomicCounter, AtomicGauge, LatencyHistogram,
    AlertRule, AlertSeverity, ThresholdCondition,
    HealthService, ComponentHealth, expose_prometheus,
    MonitorError,
)

# 1) 注册指标(原子,纳秒级)
reg = MetricsRegistry()
order_count = reg.register_counter("orders_total", labels=["side", "symbol"])
order_latency = reg.register_histogram("order_latency_ns",
                                        buckets=(1000, 10_000, 100_000, 1_000_000, 10_000_000))
nav_gauge = reg.register_gauge("portfolio_nav")

# 2) 业务代码中埋点
order_count.inc(labels={"side": "buy", "symbol": "BTCUSDT"})
order_latency.observe(150_000.0)               # 150µs
nav_gauge.set(102_345.67)

# 3) 注册告警规则
reg.add_alert_rule(AlertRule.Threshold(
    metric_name="order_latency_ns",
    condition=ThresholdCondition.GreaterThan(10_000_000.0),   # 10ms
    severity=AlertSeverity.Warning,
    message="order latency P99 > 10ms",
))
# 实时触发
alerts = reg.check_alerts("order_latency_ns", value=15_000_000.0)
for a in alerts:
    print(f"[{a.severity}] {a.message}")

# 4) 健康检查(Kubernetes liveness/readiness)
health = HealthService()
health.register("oms", lambda: ComponentHealth(status="ok",
                                                latency_ms=oms.active_count()))
health.register("exchange", lambda: adapter.health_check())
status = health.check_all()
# 暴露给 K8s:
# livenessProbe:  http://localhost:9090/health
# readinessProbe: http://localhost:9090/ready

# 5) Prometheus 暴露
from http.server import HTTPServer
expose_prometheus(reg, port=9090)              # /metrics 端点
# 接入 Prometheus / Grafana 做面板
```

**Rust 侧（开发新指标类型 / 嵌入 oms/exchange 内部埋点时使用）：**

```rust
use axon_monitor::{MetricsRegistry, AlertRule, AlertSeverity, ThresholdCondition};

let mut reg = MetricsRegistry::new();
let latency = reg.register_histogram("order_latency_ns");
latency.observe(150_000.0);
reg.add_alert_rule(AlertRule::Threshold {
    metric_name: "order_latency_ns".into(),
    condition: ThresholdCondition::GreaterThan(10_000_000.0),
    severity: AlertSeverity::Warning,
    message: "order latency > 10ms".into(),
});
```

### 关键依赖
- **依赖**：`axon-core`
- **被依赖**：`axon-exchange`（延迟监控）、`axon-oms`（订单计数）、生产服务

---

## 20. `axon-defi`

### 核心职责
DeFi 链上交易：EVM RPC + 签名 + ERC-20 + Uniswap V3 路由/报价/池子 + LayerZero 跨链 + MEV-Share。

### 代码位置
- `crates/axon-defi/src/lib.rs` — 入口（`VERSION`）
- `crates/axon-defi/src/evm/provider.rs` — `EvmProvider`（RPC 客户端）
- `crates/axon-defi/src/evm/chain.rs` — `Chain` / `ChainSpec`
- `crates/axon-defi/src/evm/erc20.rs` — `Erc20`（合约绑定）
- `crates/axon-defi/src/evm/signer.rs` — 私钥签名
- `crates/axon-defi/src/evm/multicall.rs` — Multicall3 批量调用
- `crates/axon-defi/src/dex/uniswap.rs` — Uniswap V2 路由
- `crates/axon-defi/src/dex/v3_router.rs` / `v3_quoter.rs` / `v3_pool.rs` — Uniswap V3
- `crates/axon-defi/src/bridge/layerzero.rs` — LayerZero 跨链
- `crates/axon-defi/src/mev/share.rs` — MEV-Share 集成
- `crates/axon-defi/src/python/` — PyO3 绑定（**需要 `evm` feature 启用**）

### 核心机制
- **EVM provider**：基于 `ethers-rs`（feature-gated）
- **Multicall**：用 Multicall3 合约一次 RPC 拿多个只读调用的结果
- **V3 报价**：`v3_quoter::quote_exact_input_single` 估算 swap 输出
- **MEV-Share**：把交易 bundle 提交到 Flashbots，保护免受 sandwich

### 适用场景
- 链上做市 / 套利（Uniswap V2/V3）
- 大额 swap 想避免 MEV 损失（用 `mev::share`）
- 跨链桥（LayerZero）
- 钱包集成（`signer` + `erc20::transfer`）

### 不适用场景
- CEX 套利（那是 `axon-exchange`）
- 实时高频链上交易（12s 出块周期不适合 HFT）
- 非 EVM 链（暂不支持 Solana / Sui 等）

### 怎么用

**Python 侧（主用法，EVM 链上交易 5 类场景）：**

```python
import asyncio
from axon_quant.defi import (
    Chain, EvmConfig, DefiOrder, evm_provider, local_signer, erc20_client,
    V3Quoter, Multicall, BridgeManager, MevShareClient,
    DefiError,
)
# 注:defi 模块需 `evm` feature 启用

# ─── 1) Provider + 查询链上状态 ─────────────────────────
provider = evm_provider(Chain.Ethereum, "https://eth.llamarpc.com")
print(await provider.chain_id())       # 1
print(await provider.block_number())   # 当前块高

# ─── 2) ERC-20 余额查询(走 Multicall 批量)─────────────────
usdc = erc20_client("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", provider)
print(await usdc.symbol())             # "USDC"
print(await usdc.decimals())           # 6
print(await usdc.balance_of("0xYourAddress"))

# Multicall:一次 RPC 查 N 个地址
mc = Multicall(provider)
balances = await mc.balance_of_batch(usdc, [
    "0xAddr1", "0xAddr2", "0xAddr3",
])
# balances: ['1000000000', '500000000', '0']

# ─── 3) Uniswap V3 报价(只读,不发交易)─────────────────────
quoter = V3Quoter(provider)
amount_out = await quoter.quote_exact_input_single(
    token_in="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",   # WETH
    token_out="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",  # USDC
    fee_tier=3000,                                          # 0.3%
    amount_in="1000000000000000000",                        # 1 WETH
)
print(f"1 WETH ≈ {amount_out} USDC")

# ─── 4) 真实 Swap(写链,需 signer + V3 Router)────────────────
signer = local_signer(private_key="0xYourPrivateKey")
order = DefiOrder.swap_exact_in(
    token_in="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
    token_out="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
    fee_tier=3000,
    amount_in="1000000000000000000",
    min_amount_out=amount_out * 0.99,        # 1% 滑点保护
    recipient=signer.address,
    deadline=int(time.time()) + 300,
)
tx_hash = await signer.send_v3_swap(provider, order)
print(f"交易已发: https://etherscan.io/tx/{tx_hash}")

# ─── 5) 大额 swap 防 MEV:走 Flashbots MEV-Share ─────────
mev = MevShareClient(auth_signer=signer, endpoint="https://relay.flashbots.net")
tx_hash = await mev.submit_transaction(signed_tx_bytes, ...)
# bundle 模式自动保护免受 sandwich

# ─── 6) 跨链桥(LayerZero V2)──────────────────────────────
bridge = BridgeManager()
print(bridge.is_supported(src_chain=Chain.Ethereum, dst_chain=Chain.Arbitrum))
```

**Rust 侧（开发新 EVM 适配器时使用）：**

```rust
#[cfg(feature = "evm")]
use axon_defi::{EvmProvider, Chain, Erc20};

let provider = EvmProvider::connect(Chain::Ethereum, "https://eth.llamarpc.com").await?;
let usdc = Erc20::new(USDC_ADDR, provider.clone());
let balance = usdc.balance_of(my_addr).await?;
```

### 关键依赖
- **依赖**：`axon-core`、ethers / alloy（feature-gated）
- **被依赖**：DeFi 策略研究、跨链桥集成

---

## 21. `axon-harness`

### 核心职责
Harness 编排系统的 **trait 接口 + 安全组件**：熔断器、审计链、仓位守卫 + 默认裁决策略 + RBAC 工具门控 + Token 预算守卫 + 可观测性。

### 代码位置
- `crates/axon-harness/src/lib.rs` — 入口
- `crates/axon-harness/src/policy.rs` — `HarnessPolicy` / `ToolGate` / `BudgetGuard` trait
- `crates/axon-harness/src/types.rs` — `AgentIntent` / `TaskContext` / `HarnessResult`
- `crates/axon-harness/src/default_policy.rs` — `DefaultPolicy`（组合 ToolGate + BudgetGuard + Risk）
- `crates/axon-harness/src/simple_budget.rs` — `SimpleBudgetGuard`（Token 用量限额）
- `crates/axon-harness/src/rbac_gate.rs` — `RBACToolGate`（按角色控制工具访问）
- `crates/axon-harness/src/bridge.rs` — `HarnessBridge`（把 LLM Agent 接入 Harness）
- `crates/axon-harness/src/observer.rs` — `HarnessObserver`（决策记录 / 指标）
- `crates/axon-harness/src/circuit_breaker.rs` — `CircuitBreaker`（AtomicU8 状态机，< 20ns）
- `crates/axon-harness/src/audit.rs` — `AuditChain`（Blake3 哈希链）
- `crates/axon-harness/src/position.rs` — `PositionGuard`

### 核心机制
- **三段式守卫**：`HarnessBridge` 在 Agent 每次工具调用前过：
  1. `RBACToolGate`（角色有权调？）
  2. `SimpleBudgetGuard`（token 还有预算？）
  3. `PositionGuard` + `CircuitBreaker`（仓位/熔断允许？）
- **Blake3 审计链**：`AuditChain::append(entry) -> entry`，哈希 = `blake3(prev_hash || payload)`
- **观测**：`HarnessObserver::record_decision` 写指标 + 决策到 `axon-tracker`

### 适用场景
- LLM Agent 工具调用的统一安全层（`axon-llm` 默认走这个）
- 任何需要按角色 / 预算 / 风控门控的 Agent 编排
- 多 Agent 协作时把每个 Agent 的决策记录到 `AuditChain`
- 与 `axon-risk` 联动（Risk 拒绝时同时进 `AuditChain`）

### 不适用场景
- 不需要安全门控的内部脚本
- 单机非 Agent 系统（直接用 `axon-oms` 即可）
- 跨进程事务（审计链是单进程内的，多进程需要外部存储）

### 怎么用

**Python 侧（主用法，3 类场景）：**

```python
from axon_quant.harness import (
    HarnessBridge, HarnessPolicy, DefaultPolicy,
    RBACToolGate, SimpleBudgetGuard, PositionGuard,
    CircuitBreaker, AuditChain, HarnessObserver,
    PlaceOrderTool, QueryPortfolioTool, CancelOrderTool, ReplaceOrderTool,
    MockTradingBackend, RiskLimits,
)
from axon_quant.harness.tools import ToolRole  # 角色枚举

# 1) 构造默认策略(RBAC + 预算 + 风控 三段门控)
policy = DefaultPolicy(
    tool_gate=RBACToolGate.strict(allowed_roles={ToolRole.Trader}),
    budget_guard=SimpleBudgetGuard(max_tokens=100_000),   # LLM 预算
    position_guard=PositionGuard(max_position_per_symbol=100.0,
                                 max_leverage=3.0),
    circuit_breaker=CircuitBreaker(max_consecutive_losses=5,
                                   cooldown_seconds=300),
)
bridge = HarnessBridge(policy=policy, observer=HarnessObserver())

# 2) 注册工具(每个工具在调用前都过三段门控)
backend = MockTradingBackend()    # 实际生产接 axon-exchange
risk_limits = RiskLimits(
    max_order_notional=10_000.0,
    max_daily_orders=100,
    allowed_symbols=["BTC-USDT", "ETH-USDT"],
)
place_tool = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk_limits)
query_tool = QueryPortfolioTool(backend=backend)
cancel_tool = CancelOrderTool(backend=backend, risk=risk_limits)
replace_tool = ReplaceOrderTool(backend=backend, risk=risk_limits)

bridge.register_tool("place_order", place_tool, role=ToolRole.Trader)
bridge.register_tool("query_portfolio", query_tool, role=ToolRole.Trader)
bridge.register_tool("cancel_order", cancel_tool, role=ToolRole.Trader)

# 3) 接入 LLM Agent(ReAct / Swarm)自动走门控
# 任何 LLM 工具调用前,HarnessBridge 自动:
#  a) 角色检查(LLM 当前角色有权调?)
#  b) 预算检查(token 预算还够?)
#  c) 风控检查(仓位/熔断允许?)
#  d) 审计留痕(Blake3 哈希链追加)

# 4) 手动调用(走同一套门控 + 审计)
result = bridge.invoke(
    tool_name="place_order",
    caller_role=ToolRole.Trader,
    args={"symbol": "BTC-USDT", "side": "Buy", "quantity": 0.1, "price": 50000.0},
)
print(result.success, result.output)

# 5) 审计链验证(崩溃恢复后必跑)
audit: AuditChain = bridge.audit_chain()
assert audit.verify_integrity()    # 哈希链未被篡改
print(f"审计事件数: {audit.event_count}")

# 6) 可观测性(决策记录 → 推到 axon-tracker)
observer: HarnessObserver = bridge.observer()
observer.export_to_tracker(axon_quant.tracker.MemoryTracker())
```

**Rust 侧（开发新 Gate / 新 Policy 时使用）：**

```rust
use axon_harness::{HarnessBridge, DefaultPolicy, RBACToolGate, SimpleBudgetGuard};

let policy = DefaultPolicy::new()
    .with_tool_gate(RBACToolGate::strict("trader"))
    .with_budget(SimpleBudgetGuard::new(100_000));
let bridge = HarnessBridge::new(policy);
let result = bridge.invoke("place_order", args)?;
```

### 关键依赖
- **依赖**：`axon-core` / `blake3`
- **被依赖**：`axon-llm`（Agent 工具调用门控）、`axon-oms`（实际下单）

---

## 22. `axon-integration-tests`

### 核心职责
跨 crate 端到端测试 + 属性测试 + 契约测试。仅测试时编译，不进 release。

### 代码位置
- `crates/axon-integration-tests/src/lib.rs` — 入口
- `crates/axon-integration-tests/src/matching_flow.rs` — 场景 1：回测撮合
- `crates/axon-integration-tests/src/hpo_flow.rs` — 场景 3：HPO 全流程
- `crates/axon-integration-tests/src/walkforward_flow.rs` — 场景 4：Walk-Forward
- `crates/axon-integration-tests/src/distributed_flow.rs` — 场景 6：分布式训练
- `crates/axon-integration-tests/src/tracker_registry_flow.rs` — 场景 5：追踪 + 注册
- `crates/axon-integration-tests/src/e2e_pipeline.rs` — 端到端 4-crate 串联
- `crates/axon-integration-tests/src/phase4_e2e.rs` — Phase 4（生产部署链路）
- `crates/axon-integration-tests/src/contract.rs` — API/数据契约稳定性
- `crates/axon-integration-tests/src/fuzz.rs` — proptest 属性测试
- `crates/axon-integration-tests/src/fixtures.rs` — 共享 fixture

### 核心机制
- **场景化组织**：每个 `_flow.rs` 对应一个业务场景，串联多个 crate
- **Property-based**：用 `proptest` 自动生成输入，验证不变量
- **契约测试**：snapshot 序列化结果，跨版本检查兼容性

### 适用场景
- 跑 `cargo test -p axon-integration-tests` 验证全链路
- 添加新模块时写对应 `_flow.rs` 场景
- CI 阻断（见 `.github/workflows/validation.yml`）

### 不适用场景
- 单 crate 单元测试（用各 crate 自己的 `tests/` 目录）
- 性能基准（用 `benches/`）
- 真实交易所 e2e（那是 e2e-real-llm.yml workflow）

### 怎么用

> `axon-integration-tests` 是 **Rust 端跨 crate 集成测试框架**,不在 Python wheel 中暴露。
> Python 用户通常不需要直接用本模块;若想跑全链路回归,在仓库根目录执行:

```bash
# 跑所有集成测试(本地开发)
cargo test -p axon-integration-tests --features all

# 跑指定场景(例如 e2e_pipeline 端到端 4-crate 串联)
cargo test -p axon-integration-tests --test e2e_pipeline

# 跑 proptest 属性测试
cargo test -p axon-integration-tests fuzz

# CI 阻断
.github/workflows/validation.yml   # 自动跑全部场景
```

**Rust 侧（开发新场景 / 给新模块写 _flow 时使用）：**

```rust
use axon_integration_tests::fixtures;

#[test]
fn my_new_flow() {
    let (oms, risk, mock_exchange) = fixtures::full_stack();
    // 1. 准备:回测数据 + 风险配置
    // 2. 跑场景:回测 -> HPO -> 评估 -> 注册
    // 3. 断言:全链路 Sharpe > baseline
}
```

### 关键依赖
- **依赖**：几乎所有 `axon-*` crate
- **被依赖**：CI workflow

---

## 23. `axon-python`

### 核心职责
Python 统一入口 `axon_quant._native`：把各 crate 的 PyO3 绑定 + 共享异常基类聚合到一个模块。

### 代码位置
- `crates/axon-python/src/lib.rs` — `#[pymodule] _native` 入口
- `crates/axon-python/src/error.rs` — 公共异常基类 `AxonError` + 6 个子类
- `crates/axon-python/src/harness.rs` — Harness Python 绑定

### 核心机制
- **统一异常**：`register_exceptions` 在子模块 `create_exception!` **之前**注册基类
- **特性门控**：仅 `python` feature 开启时编译（`#![cfg(feature = "python")]`）
- **避免循环**：不依赖各 crate 的 `python` 子模块（它们独立注册到子模块名）

### 适用场景
- Python 用户 `import axon_quant` 后获得全部能力
- 用 `pip install axon-quant` 安装 wheel 后能 `import axon_quant.rl` 等
- 在 PyO3 0.28 + Python 3.12+ 环境下运行

### 不适用场景
- 纯 Rust 项目（直接用各 crate，不需要这个聚合）
- 嵌入式环境（Python 解释器太重）

### 怎么用

```python
import axon_quant
print(axon_quant.__version__)  # 0.4.0

# 子模块
env = axon_quant.rl.TradingEnv(...)
df = axon_quant.data.CsvSource(...)
risk = axon_quant.risk.DefaultRiskEngine(...)
```

### 关键依赖
- **依赖**：所有带 `python` feature 的 crate
- **被依赖**：Python 用户代码、PyPI 发布

---

## 模块依赖速查

```text
axon-core ◄── axon-backtest ◄── axon-rl
        ▲                ▲           │
        │                │           ├── axon-hpo
        │                │           ├── axon-walk-forward
        │                │           ├── axon-distributed
        │                │           └── axon-tracker
        │                │           │
        │                │           └── axon-registry
        │                │
        │                ├── axon-data
        │                ├── axon-llm ───► axon-explain
        │                │       │
        │                │       └──► axon-oms ◄── axon-risk
        │                │                  ▲
        │                │                  │
        │                └──► axon-exchange ┘
        │
        ├── axon-compliance
        ├── axon-monitor
        ├── axon-inference
        ├── axon-defi
        ├── axon-harness ──► axon-llm / axon-oms
        └── axon-integration-tests (test-only, 依赖所有)
              ▲
              │
        axon-python (聚合上述带 python feature 的 crate)
```

---

## 模块选择决策树

| 你想做的事 | 用哪个模块 |
|-----------|-----------|
| 实现可重现的回测 | `axon-backtest::BacktestEngine` |
| 训练 RL 策略 | `axon-rl::TradingEnv` + `axon-tracker` |
| 跑超参搜索 | `axon-hpo` + `axon-tracker` |
| 滚动前向验证 | `axon-walk-forward` |
| 多机多卡训练 | `axon-distributed` |
| 跟踪实验指标 | `axon-tracker` |
| 管理模型版本 | `axon-registry` |
| LLM 智能体交易 | `axon-llm` + `axon-harness` |
| 解释模型决策 | `axon-explain` |
| 多策略融合 | `axon-ensemble` |
| 接历史数据 | `axon-data` |
| 合规留痕 | `axon-compliance` |
| 预交易风控 | `axon-risk` |
| 推理服务 | `axon-inference` |
| 实盘下单 | `axon-exchange` + `axon-oms` + `axon-risk` |
| 订单生命周期 | `axon-oms` |
| 生产监控告警 | `axon-monitor` |
| 链上交易 | `axon-defi` |
| Agent 安全门控 | `axon-harness` |
| Python 入口 | `axon-python` |
| 跨模块端到端测试 | `axon-integration-tests` |
