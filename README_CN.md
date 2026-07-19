<div align="center">

# <img src="docs/assets/logo.svg" width="36" alt="" style="vertical-align: middle;"/> AXON

**AI 原生量化交易框架**

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](./LICENSE) [![Rust](https://img.shields.io/badge/Rust-1.96%2B-orange.svg)](https://www.rust-lang.org/) [![Python](https://img.shields.io/badge/Python-3.14%2B-3776AB.svg)](https://www.python.org/) [![Version](https://img.shields.io/badge/Version-0.7.1-green.svg)](./CHANGELOG.md) [![CI](https://img.shields.io/github/actions/workflow/status/pengwow/axon_quant/validation.yml?label=CI)](https://github.com/pengwow/axon_quant/actions) [![Tests](https://img.shields.io/badge/Tests-3000%2B-brightgreen.svg)](./crates/)

**[English](./README.md)** | 中文


</div>

> 面向量化交易与强化学习的事件驱动交易引擎。它从设计之初就以 AI 为核心，而非在传统量化系统上"嫁接"机器学习模块。

Rust 实现高性能内核，Python 提供 RL 训练接口，一套代码贯穿回测、训练、优化、验证、生产的完整链路。

[在线文档](https://pengwow.github.io/axon_quant/) · [示例](./examples/)

## 设计哲学

- **AI First**：强化学习（RL）环境与回测引擎共享同一套数据结构，训练与生产零差异
- **Rust Core**：纳秒级时间戳、确定性撮合、零成本抽象，回测吞吐 > 1M events/sec
- **Python Front**：通过 PyO3 暴露 Gymnasium 兼容接口，可直接挂 Stable-Baselines3 / Ray RLlib
- **Full Pipeline**：回测 → 训练 → HPO → Walk-forward → 追踪 → 注册 → 部署，全链路内置
- **100% 开源**：Apache-2.0 许可，无企业版、无功能阉割

***

## 特性

### 回测引擎

- **多级撮合**：L1 基础撮合 → L2 复杂订单簿 → L3 多资产交叉
- **冲击模型**：Almgren-Chriss 永久/临时冲击 + 概率延迟 + 分层费用
- **确定性回放**：`SimulatedClock` + crossbeam-channel bounded 100K 事件队列
- **列式存储**：Arrow/Parquet，1M tick 读写 < 15ms
- **流式引擎**：通过 `StreamDataSource` trait 实时接入 tick，支持 CSV 回放 / 交易所模拟 / 未来 WebSocket；`StreamingStrategy` trait 驱动 tick 级策略；`StreamingMetrics` 实时追踪权益曲线、夏普比率、最大回撤、胜率
- **模拟盘**：可配置滑点、成交概率、部分成交比例的模拟交易所；确定性 RNG 种子保证测试可重复

### RL 环境

- **Gymnasium API**：离散 / 连续 / 混合动作空间
- **奖励函数**：PnL / Sharpe / Sortino，基于统一 `ReturnHistory`
- **向量化**：`VecEnv` 支持多环境并行 rollout
- **PyO3 绑定**：maturin 打包，6 个子模块

### 训练管线

- **超参优化**：Optuna 集成 + NSGA-II 多目标 + Pareto 前沿 + 早停剪枝
- **滚动前向验证**：Purged + Embargo + 泄漏检测 + Deflated Sharpe Ratio
- **实验追踪**：MLflow / WandB / Local / Memory 四后端
- **模型注册**：SemVer + 阶段生命周期 + 自动归档 + 回滚
- **分布式训练**：Ray Actor + Parameter Server + Checkpoint 容错

### AI 增强

- **LLM 智能体**：ReAct + Tool Calling，内置 `PlaceOrder` / `QueryPortfolio` 交易工具，带 SafetyMode 风控
- **Agent Swarm**：多 Agent 协作框架，采用 Actor 模型，支持投票共识和动态扩缩容
  - **MarketAgent**：市场分析与信号生成
  - **RiskAgent**：预交易风控评估与合规检查
  - **ExecutionAgent**：订单执行（TWAP/VWAP 策略）
  - **AuditAgent**：决策日志与合规报告
  - **SwarmOrchestrator**：Agent 生命周期管理、消息路由、自动扩缩容
- **模型集成**：Voting / Stacking / 动态加权，在线监控夏普比率自动调权
- **可解释性**：SHAP 特征归因 + 反事实解释 + `Explainer` trait 内建
- **合规审计**：不可篡改的交易日志 + 决策报告归档

### 生产部署

- **交易所适配**：Binance / OKX REST + WebSocket（自动重连）
- **风控引擎**：预交易检查（12ns）、实时熔断、仓位限制
- **推理引擎**：ONNX / Candle 双后端 + CPU/GPU 亲和性绑核 + 批推理

### DeFi 集成（实验性）

> **注意**：DeFi 功能为实验性质，正在积极开发中，API 可能会变化。

- **EVM 链支持**：Ethereum / Arbitrum / Optimism / Polygon
- **DEX 集成**：Uniswap V3 直接集成，最优路由
- **MEV 保护**：MEV-Share 防止三明治攻击
- **智能合约风控**：混合风控检查（链下快速 + 链上权威）
- **跨链桥接**：LayerZero 集成，支持多链资产转移

***

## 快速开始

### 安装（推荐）

```bash
# 基础安装（核心 + 数据处理）
pip install axon_quant

# 包含 ONNX 推理支持（onnxruntime，自动加载）
pip install axon_quant[onnx]

# 包含 RL 训练依赖（gymnasium, stable-baselines3, torch）
pip install axon_quant[rl]

# 全功能安装
pip install axon_quant[onnx,rl]
```

验证安装：

```bash
python -c "import axon_quant; print(axon_quant.__version__)"
```

### 从源码构建

如需修改 Rust 核心代码：

```bash
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# 编译
cargo build

# 测试（2300+ 用例）
cargo test --workspace

# 静态检查
cargo clippy --workspace -- -D warnings

# 构建并安装 Python wheel
maturin build --release
pip install target/wheels/axon_quant-*.whl
```

### 训练示例

```bash
# 随机基线
python examples/01_random_agent.py

# PPO 训练
python examples/02_train_ppo.py --timesteps 50000

# HPO 优化
python examples/03_hpo/hpo_single_objective.py

# 滚动前向验证
python examples/08_walk_forward/walk_forward_basic.py
```

> 📖 详细的 RL 训练文档请参考：[RL 训练指南](docs/zh/user-guide/rl-training.md)

***

## 架构

AXON 采用 Cargo Workspace 管理 21 个 crate，按依赖层级自下而上分为 9 层：

```
┌─────────────────────────────────────────────────────────────┐
│  Layer 9: 应用入口                                             │
│  ├─ axon-cli        CLI 工具                                  │
│  └─ axon-python     PyO3 统一入口（axon_quant 包）              │
├─────────────────────────────────────────────────────────────┤
│  Layer 8: AI 智能体                                            │
│  ├─ axon-llm        ReAct 智能体 + Tool Calling               │
│  └─ axon-explain    SHAP / 反事实 / 决策报告                   │
├─────────────────────────────────────────────────────────────┤
│  Layer 7: 模型服务                                             │
│  ├─ axon-inference  ONNX / Candle / tch 推理引擎              │
│  └─ axon-ensemble   模型集成（投票 / Stacking / 动态加权）      │
├─────────────────────────────────────────────────────────────┤
│  Layer 6: 训练管线                                             │
│  ├─ axon-rl         Gymnasium 环境 + VecEnv + 奖励函数         │
│  ├─ axon-hpo        Optuna 超参优化（单目标 / 多目标）          │
│  ├─ axon-distributed Ray Actor 分布式训练                     │
│  └─ axon-walk-forward 滚动前向验证（Purged + Embargo）         │
├─────────────────────────────────────────────────────────────┤
│  Layer 5: 实验治理                                             │
│  ├─ axon-tracker    MLflow / WandB / Local / Memory 追踪      │
│  └─ axon-registry   模型注册表（SemVer + 生命周期 + 回滚）      │
├─────────────────────────────────────────────────────────────┤
│  Layer 4: 生产执行                                             │
│  ├─ axon-exchange   Binance / OKX 适配器（REST + WebSocket）   │
│  ├─ axon-risk       风控引擎（仓位 / 回撤 / VaR / 熔断）        │
│  ├─ axon-oms        订单管理系统                               │
│  └─ axon-monitor    监控告警 + 健康检查                        │
├─────────────────────────────────────────────────────────────┤
│  Layer 3: 回测引擎                                             │
│  ├─ axon-backtest   L1/L2/L3 撮合 + Almgren-Chriss 冲击模型    │
│  └─ axon-compliance 合规审计 + 日报 / 月报 / 年报               │
├─────────────────────────────────────────────────────────────┤
│  Layer 2: 数据服务                                             │
│  └─ axon-data       Arrow 列式存储 + CSV/Parquet 源 + 特征管道  │
├─────────────────────────────────────────────────────────────┤
│  Layer 1: 核心类型                                             │
│  └─ axon-core       时间戳 / 价格 / 数量 / 订单 / 事件 / 队列   │
│                     / 组合 / 波动率 / 延迟 / 费用 / SIMD        │
└─────────────────────────────────────────────────────────────┘
```

### 线程模型

- **核心匹配引擎**：单线程，避免锁竞争，保证确定性
- **I/O 线程池**：tokio runtime，处理 WebSocket / REST / 文件 I/O
- **计算线程池**：rayon，因子计算 / 数据转换 / 并行回测
- **事件队列**：crossbeam-channel bounded 100K，零锁设计

### 数据管道

AXON 的所有模块共享同一套 Arrow `RecordBatch`，零拷贝透传，无格式转换断层：

```
数据源 (CSV/Parquet/WebSocket/Mock/交易所 API)
    │
    ▼
axon-data (schema 校验 / 时间对齐 / 去重 / mmap 缓存)
    │
    ▼
Arrow RecordBatch (内存) ──→ TradingEnv / FeaturePipeline / BacktestEngine
    │
    ▼
InferenceEngine (ONNX/Candle 批推理 < 1ms)
    │
    ▼
ExchangeAdapter (Binance/OKX 实盘下单)
```

### 层级说明

1. **axon-core**：整个系统的基石。提供 `Timestamp`（纳秒精度）、`Price` / `Quantity`（基于 `rust_decimal`）、`Order`、`Event`、`Queue`、`Portfolio` 等核心类型，以及 SIMD 加速的归一化与订单簿操作。
2. **axon-data**：统一数据接入层。基于 Apache Arrow 的 `RecordBatch` 列式存储，支持 CSV / Parquet / Mock 数据源，内置 `FeaturePipeline`（Z-Score 归一化 + 滑动窗口）。
3. **axon-backtest**：事件驱动回测引擎。支持 L1（价格优先）、L2（订单簿）、L3（暗池 / 拍卖）三级撮合，集成 Almgren-Chriss 市场冲击模型与概率延迟模拟。
4. **axon-exchange**：生产级交易所适配器。统一 `ExchangeAdapter` trait，已实现对 Binance / OKX 的 REST + WebSocket 对接，内置指数退避重连与令牌桶限流。
5. **axon-rl**：强化学习环境。`TradingEnv` 实现 Gymnasium 标准接口（`reset` / `step` / `render`），支持连续动作（目标仓位比例 `[-1, 1]`）、离散动作（分仓档位）、多目标奖励与向量化并行环境 `VecEnv`。
6. **axon-inference**：模型推理引擎。支持 ONNX Runtime、Candle（纯 Rust）、tch-rs（PyTorch C++）三后端，具备异步批推理管线、CPU/GPU 亲和性绑定与模型热更新能力。
7. **axon-llm**：大语言模型智能体。基于 ReAct 推理循环，内置"市场分析"、"查询持仓"、"提交订单"三个工具，支持 OpenAI 兼容后端与流式响应。
8. **axon-explain**：可解释性引擎。集成 SHAP 特征归因、反事实解释（"如果当时不买入，收益会如何变化"）与结构化决策报告，满足监管合规与策略迭代需求。
9. **axon-ensemble**：模型集成。提供 HardVote、SoftVote、WeightedVote、Stacking、DynamicWeighted 五种策略，支持在线性能监控与自动权重调整。

***

## 仓库结构

```
axon_quant/
├── crates/                     # 21 个 Rust crate
│   ├── axon-core/              # 核心类型（time/types/market/order/event/queue/portfolio）
│   ├── axon-backtest/          # 回测引擎（L1/L2/L3 撮合 + 冲击模型）
│   ├── axon-rl/                # RL 环境（Gymnasium + VecEnv）
│   ├── axon-hpo/               # 超参数优化（Optuna + NSGA-II）
│   ├── axon-walk-forward/      # 滚动前向验证（Purged + Embargo）
│   ├── axon-distributed/       # 分布式训练（Ray）
│   ├── axon-tracker/           # 实验追踪（MLflow/WandB/Local/Memory）
│   ├── axon-registry/          # 模型注册表（SemVer + 生命周期）
│   ├── axon-exchange/          # 交易所适配器（Binance/OKX）
│   ├── axon-inference/         # 推理引擎（ONNX/Candle）
│   ├── axon-risk/              # 风控引擎
│   ├── axon-oms/               # 订单管理系统
│   ├── axon-monitor/           # 监控告警
│   ├── axon-llm/               # LLM 智能体
│   ├── axon-python/            # Python 绑定入口
│   └── axon-cli/               # CLI 工具
├── python/                     # Python 包（axon_quant）
├── examples/                   # 训练示例脚本
├── tests/                      # 测试（Rust + Python）
├── docs/                       # 设计文档 + ADR
├── scripts/                    # 构建与测试脚本
├── pyproject.toml              # Python 打包配置
├── Makefile                    # 开发命令
└── Dockerfile                  # 多阶段构建
```

***

## Crate 矩阵

| Crate                  | 功能                        |
| ---------------------- | ------------------------- |
| axon-core              | 核心类型（11 模块）               |
| axon-backtest          | 回测引擎（L1/L2/L3）            |
| axon-rl                | RL 环境（Gymnasium + VecEnv） |
| axon-hpo               | 超参数优化（Optuna）             |
| axon-walk-forward      | 滚动前向验证                    |
| axon-distributed       | 分布式训练（Ray）                |
| axon-tracker           | 实验追踪                      |
| axon-registry          | 模型注册表                     |
| axon-exchange          | 交易所适配器（Binance/OKX）       |
| axon-inference         | 推理引擎（ONNX/Candle）         |
| axon-python            | Python 绑定（PyO3）           |
| axon-cli               | CLI 工具                    |
| axon-risk              | 风控引擎                      |
| axon-oms               | 订单管理                      |
| axon-monitor           | 监控告警                      |
| axon-llm               | LLM 智能体                   |
| axon-explain           | SHAP 可解释性                 |
| axon-ensemble          | 模型集成                      |
| axon-compliance        | 合规审计                      |
| axon-data              | 数据服务                      |
| axon-integration-tests | 集成测试                      |

***

## 性能

| 指标    | 数值                                |
| ----- | --------------------------------- |
| 回测吞吐  | > 1M events/sec                   |
| 撮合延迟  | < 1us (P99)                       |
| 风控检查  | 12ns (AtomicBool 熔断 + HashMap 仓位) |
| 订单提交  | 1.2µs (幂等 + UUID v7 + 状态机)        |
| RL 训练 | > 10k steps/sec (8 env VecEnv)    |
| 分布式加速 | > 5x (8 workers)                  |
| 测试用例  | 1200+ Rust + 24 Python            |

### 基准测试

workspace 已建立 50+ Criterion bench，跨 5 个 crate:

| Crate           | Bench 入口                     | 覆盖                                                                   |
| --------------- | ---------------------------- | -------------------------------------------------------------------- |
| `axon-core`     | `benches/core_bench.rs`      | 28 个:冲击模型/波动率/延迟/订单簿/订单/事件/费用                                        |
| `axon-backtest` | `benches/impact_bench.rs`    | 8 个:撮合延迟/不同冲击模型/订单簿深度/永久衰减/多笔/TOML 配置                                |
| `axon-data`     | `benches/axon_data_bench.rs` | 7 个 group(8+ bench):LRU/Dataset lazy/CSV/Parquet 流式/Bar 聚合/Mock/Mmap |
| `axon-rl`       | `benches/rl_bench.rs`        | 11 个:观测/奖励/TradingEnv 端到端/Action 转换                                  |
| Phase 4 crates  | `benches/phase4_bench.rs`    | 15 个:风控/OMS/监控延迟                                                     |

```bash
make bench                 # 全 workspace,本地 5-10 分钟
make bench-cmp             # 存 main baseline,PR 对比
make bench-one CRATE=axon-core BENCH=event_builder_tick   # 单个 bench
cargo bench -p axon-core -- impact_linear    # 直接 cargo 跑
```

CI 不跑 bench（避免 main runner 性能噪声）。报告: `target/criterion/<group>/report/index.html`。

### CPU/GPU 亲和性

`axon-inference` 提供 `affinity` 模块，跨平台绑核降低跨核 cache miss:

```rust
use axon_inference::affinity::{AffinityPlan, pin_to};
let plan = AffinityPlan::new().with_cpus(vec![0, 1]).with_cuda(0);
pin_to(&plan)?;
```

或通过 `BatchConfig` 配置（`BatchInferencePipeline::new` 启动时自动调）:

```toml
[batch]
collect_cpu_cores = [0, 1, 2, 3]
collect_gpu_device_id = 0
```

平台支持: Linux / macOS 完整支持, Windows 运行时返回 `Err(AffinityError::NotAvailable)`（用 WSL2 / numactl 替代）。

***

## 工程实践

- **TDD 驱动** — 先测试后实现，CI 强制 `-D warnings`
- **1200+ 测试** — 单元测试 + 集成测试 + Python 场景测试
- **cargo clippy** — 零警告策略
- **cargo-mutants** — 变异测试覆盖
- **cargo-fuzz** — 模糊测试（撮合引擎/订单簿/风控）
- **Miri** — 数据竞争检测
- **Loom** — 确定性并发测试

***

## 文档

- [安装与快速入门](docs/zh/getting-started/installation.md)
- [AI 原生核心设计](docs/zh/user-guide/ai-native-design.md)
- [策略研发全流程](docs/zh/user-guide/strategy-development.md)
- [LLM 智能体驱动交易](docs/zh/user-guide/llm-trading/oader.md)
- [生产部署与监控](docs/zh/user-guide/production.md)
- [传统策略迁移](docs/zh/user-guide/traditional-strategy.md)
- [API 参考](docs/zh/reference/api-reference.md)
- [常见问题](docs/zh/about/faq.md)

***

## 许可

[Apache-2.0](./LICENSE)

---

## 免责声明

本项目是一个**开源量化交易框架**，仅供**研究和学习目的**使用。

- **非投资建议**：本仓库中的任何内容均不构成金融、投资或交易建议。
- **不保证收益**：历史表现（包括回测结果）不代表未来收益。
- **风险自担**：作者和贡献者**不对使用本软件造成的任何经济损失承担责任**。
- **非生产就绪**：本软件按"现状"提供，不附带任何明示或暗示的保证。在实盘环境中使用前，需进行充分测试和风险评估。
- **合规责任**：用户有责任自行确保其使用行为符合适用的法律法规。

**使用本软件即表示您理解并接受上述条款。**
