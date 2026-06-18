# AXON

> 面向量化交易与强化学习的开源事件驱动交易引擎
> Rust 核心 + Python 接口，从回测到生产的全链路统一框架

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.96.0%2B-orange.svg)](./rust-toolchain.toml)
[![CI](https://img.shields.io/badge/CI-validation-blue?logo=githubactions&logoColor=white)](./.github/workflows/validation.yml)

[设计文档](./axon-design/) · [ADR](./docs/adr/) · [更新日志](./CHANGELOG.md) · [示例](./examples/)

---

## 什么是 AXON

AXON 是一个面向量化交易与强化学习的事件驱动交易引擎。Rust 实现高性能内核，Python 提供 RL 训练接口，一套代码贯穿回测、训练、优化、验证、生产的完整链路。

**核心特性：**

- **AI 原生**：内置 Gymnasium 兼容 RL 环境，可直接挂 Stable-Baselines3 / Ray RLlib
- **Rust 高性能**：纳秒级时间戳、确定性撮合、零成本抽象
- **全链路统一**：回测、训练、优化、验证、追踪、注册共用一套数据结构
- **模块化**：21 个 crate 独立可编译，feature flag 按需启用
- **100% 开源**：Apache-2.0 许可，无企业版、无功能阉割

---

## 项目状态

| 里程碑 | 内容 | 状态 |
|--------|------|------|
| M0 | 项目骨架 + CI | 完成 |
| M1 | 回测引擎（L1/L2/L3 撮合 + 冲击/延迟/费用） | 完成 |
| M2 | RL 环境（Gymnasium + VecEnv + 6 示例） | 完成 |
| M3 | 训练管线（HPO + Walk-forward + Tracker + Registry + Distributed） | 完成 |
| M4 | 生产就绪（交易所 + 风控 + OMS + 监控） | 进行中 |
| M5 | AI 高级功能（LLM + 集成 + 可解释性） | 完成 |

### 已覆盖能力

- **回测引擎**：L1/L2/L3 撮合 + Almgren-Chriss 冲击模型 + 概率延迟 + 分层费用
- **RL 环境**：Gymnasium API + 离散/连续/混合动作 + PnL/Sharpe/Sortino 奖励 + VecEnv
- **超参数优化**：Optuna 集成 + NSGA-II 多目标 + Pareto 前沿 + 早停剪枝
- **滚动前向验证**：Purged + Embargo + 泄漏检测 + Deflated Sharpe Ratio
- **实验追踪**：MLflow / WandB / Local / Memory 四后端
- **模型注册表**：SemVer + 阶段生命周期 + 自动归档 + 回滚
- **分布式训练**：Ray Actor + Parameter Server + Checkpoint 容错
- **交易所适配器**：Binance / OKX REST + WebSocket（自动重连）
- **Python 绑定**：PyO3 0.28，maturin 打包，6 个子模块

---

## 快速开始

### 环境要求

- Rust >= 1.96.0（[rustup](https://rustup.rs)）
- Python >= 3.12（可选，用于 RL 训练）

### 编译与测试

```bash
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# 编译
cargo build

# 测试（1200+ 用例）
cargo test --workspace

# 静态检查
cargo clippy --workspace -- -D warnings
```

### Python Wheel

```bash
# 构建 wheel
maturin build --release

# 安装
pip install target/wheels/axon_quant-*.whl

# 验证
python -c "import axon_quant; print(axon_quant.__version__)"
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

---

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

---

## Crate 矩阵

| Crate | 功能 | 状态 |
|-------|------|------|
| axon-core | 核心类型（11 模块） | 完成 |
| axon-backtest | 回测引擎（L1/L2/L3） | 完成 |
| axon-rl | RL 环境（Gymnasium + VecEnv） | 完成 |
| axon-hpo | 超参数优化（Optuna） | 完成 |
| axon-walk-forward | 滚动前向验证 | 完成 |
| axon-distributed | 分布式训练（Ray） | 完成 |
| axon-tracker | 实验追踪 | 完成 |
| axon-registry | 模型注册表 | 完成 |
| axon-exchange | 交易所适配器（Binance/OKX） | 完成 |
| axon-inference | 推理引擎（ONNX/Candle） | 完成 |
| axon-python | Python 绑定（PyO3） | 完成 |
| axon-cli | CLI 工具 | 完成 |
| axon-risk | 风控引擎 | 完成 |
| axon-oms | 订单管理 | 完成 |
| axon-monitor | 监控告警 | 完成 |
| axon-llm | LLM 智能体 | 完成 |
| axon-explain | SHAP 可解释性 | 完成 |
| axon-ensemble | 模型集成 | 完成 |
| axon-compliance | 合规审计 | 完成 |
| axon-data | 数据服务 | 完成 |
| axon-integration-tests | 集成测试 | 完成 |

---

## 路线图

| 阶段 | 内容 | 周期 | 状态 |
|------|------|------|------|
| Phase 0 | 架构与基础设施 | Q1 2026 | 完成 |
| Phase 1 | 核心引擎 + RL 环境 | Q1-Q2 2026 | 完成 |
| Phase 2 | 训练管线（HPO/WF/Tracker/Registry） | Q2 2026 | 完成 |
| Phase 3 | 生产部署（交易所/风控/OMS/监控） | Q2-Q3 2026 | 进行中 |
| Phase 4 | AI 高级功能（LLM/集成/可解释性） | Q3-Q4 2026 | 完成 |

---

## 性能指标

| 指标 | 数值 |
|------|------|
| 回测吞吐 | > 1M events/sec |
| 撮合延迟 | < 1us (P99) |
| RL 训练 | > 10k steps/sec (8 env VecEnv) |
| 分布式加速 | > 5x (8 workers) |
| 测试用例 | 1200+ Rust + 24 Python |

### 性能基准

workspace 已建立 50+ Criterion bench,跨 5 个 crate:

| Crate | Bench 入口 | 覆盖 |
|-------|-----------|------|
| `axon-core` | `benches/core_bench.rs` | 28 个:冲击模型/波动率/延迟/订单簿/订单/事件/费用 |
| `axon-backtest` | `benches/impact_bench.rs` | 8 个:撮合延迟/不同冲击模型/订单簿深度/永久衰减/多笔/TOML 配置 |
| `axon-data` | `benches/axon_data_bench.rs` | 7 个 group(8+ bench):LRU/Dataset lazy/CSV/Parquet 流式/Bar 聚合/Mock/Mmap |
| `axon-rl` | `benches/rl_bench.rs` | 11 个:观测/奖励/TradingEnv 端到端/Action 转换 |
| Phase 4 crates | `benches/phase4_bench.rs` | 15 个:风控/OMS/监控延迟 |

跑法:

```bash
make bench                 # 全 workspace,本地 5-10 分钟
make bench-cmp             # 存 main baseline,PR 对比
make bench-one CRATE=axon-core BENCH=event_builder_tick   # 单个 bench
cargo bench -p axon-core -- impact_linear    # 直接 cargo 跑
```

CI 不跑 bench(避免 main runner 性能噪声)。报告:`target/criterion/<group>/report/index.html`。

---

### CPU/GPU 亲和性

`axon-inference` 提供 `affinity` 模块,跨平台绑核降低跨核 cache miss:

```rust
use axon_inference::affinity::{AffinityPlan, pin_to};
let plan = AffinityPlan::new().with_cpus(vec![0, 1]).with_cuda(0);
pin_to(&plan)?;
```

或通过 `BatchConfig` 配置(`BatchInferencePipeline::new` 启动时自动调):

```toml
[batch]
collect_cpu_cores = [0, 1, 2, 3]
collect_gpu_device_id = 0
```

平台支持:Linux / macOS 完整支持,Windows 编译期拒绝(用 WSL2 / numactl 替代)。

---

## 工程实践

- **TDD 驱动**：先测试后实现，CI 强制 `-D warnings`
- **1200+ 测试**：单元测试 + 集成测试 + Python 场景测试
- **cargo clippy**：零警告策略
- **cargo-mutants**：变异测试覆盖
- **cargo-fuzz**：模糊测试（撮合引擎/订单簿/风控）
- **Miri**：数据竞争检测
- **Loom**：确定性并发测试

---

## 许可

[Apache-2.0](./LICENSE)
