# AXON Quant

> **AI 原生量化交易框架** — Rust 实现高性能内核，Python 提供 RL 训练接口，一套代码贯穿回测、训练、优化、验证、生产的完整链路。

AXON（**A**I-driven e**X**ecution and **O**rder e**N**gine）是面向量化交易与强化学习的事件驱动交易引擎。它从设计之初就以 AI 为核心，而非在传统量化系统上"嫁接"机器学习模块。

!!! note "版本信息"
    当前文档基于 AXON `v0.6.0` 编写，对应 Rust 版本 `1.96.0+`。

---

## 核心特性

<div class="grid cards" markdown>

-   :material-robot-outline: **AI 原生 RL 环境**

    ---

    内置 Gymnasium 兼容的 `TradingEnv`，支持离散 / 连续 / 混合动作空间，PnL / Sharpe / Sortino 奖励函数开箱即用。

-   :material-lightning-bolt: **Rust 高性能内核**

    ---

    纳秒级时间戳精度、L1/L2/L3 多级确定性撮合、SIMD 加速归一化，P99 撮合延迟 < 1μs。

-   :material-source-branch: **全链路统一**

    ---

    回测、训练、超参优化、滚动验证、实验追踪、模型注册共用一套 `MarketBar` / `PortfolioState` 数据结构，彻底消除"回测可用、上实盘崩"的隐患。

-   :material-package-variant-closed: **23 个独立 Crate**

    ---

    每个 crate 可独立编译、独立发布，通过 feature flag 按需启用。从最小内核 `axon-core` 到完整生产栈 `axon-exchange`，按需组合。

-   :material-brain: **LLM + RL 互补**

    ---

    `axon-llm` 提供 ReAct 智能体，支持工具调用（下单 / 查持仓 / 市场分析）；`axon-rl` 提供高频策略训练。两者通过 `axon-ensemble` 集成，实现"直觉 + 推理"双引擎。

-   :material-eye-outline: **可解释性内建**

    ---

    `axon-explain` 集成 SHAP 特征归因、反事实解释与决策报告生成，满足合规与策略迭代需求。

</div>

---

## 设计哲学

- **AI First**：强化学习（RL）环境与回测引擎共享同一套数据结构，训练与生产零差异
- **Rust Core**：纳秒级时间戳、确定性撮合、零成本抽象，回测吞吐 > 1M events/sec
- **Python Front**：通过 PyO3 暴露 Gymnasium 兼容接口，可直接挂 Stable-Baselines3 / Ray RLlib
- **Full Pipeline**：回测 → 训练 → HPO → Walk-forward → 追踪 → 注册 → 部署，全链路内置
- **100% 开源**：Apache-2.0 许可，无企业版、无功能阉割

---

## AI 原生 vs 传统量化

| 维度 | 传统量化框架 | AXON（AI 原生） |
|------|------------|----------------|
| **数据管道** | CSV / DataFrame 手动拼接，训练与实盘格式不一致 | Arrow `RecordBatch` 统一列式存储，`axon-data` 提供零拷贝 `fit` / `transform` 管道 |
| **策略编写** | 规则表达式（如 TA-Lib 指标组合）或独立脚本 | RL 策略 = 神经网络权重 + 环境交互；规则策略也可通过 `ActionDecoder` 接入同一执行器 |
| **回测与实盘** | 回测引擎与实盘引擎两套代码，常出现"回测圣杯、实盘亏损" | `TradingEnv` 底层直接调用 `axon-backtest` 撮合引擎；上实盘时仅替换 `ExchangeAdapter`，策略代码零改动 |
| **超参优化** | 外部脚本（如 Optuna 单独写）与主项目松耦合 | `axon-hpo` 内置 Optuna + NSGA-II 多目标 + Pareto 前沿 + 早停剪枝，与 `TradingEnv` 原生集成 |
| **可解释性** | 事后分析，需额外导出数据到 Jupyter 手工绘图 | `axon-explain` 在 `step()` 内部实时计算 SHAP 值，生成 `ExplanationReport` 随模型版本归档 |
| **模型部署** | 手动导出 ONNX / TorchScript，再写 C++ 服务封装 | `axon-inference` 支持 ONNX / Candle / tch 三后端，批推理管线 + 热更新，< 10ms 切换模型 |
| **多模型协作** | 无内置支持，需自行写投票 / 加权逻辑 | `axon-ensemble` 提供 HardVote / SoftVote / WeightedVote / Stacking / DynamicWeighted 五种集成策略 |
| **交易所对接** | 各交易所 SDK 独立封装，接口风格差异大 | `ExchangeAdapter` trait 统一 REST + WebSocket 接口，`axon-exchange` 已覆盖 Binance / OKX，测试网一键切换 |

---

## 架构总览

AXON 采用 Cargo Workspace 管理 23 个 crate，按依赖层级自下而上分为 9 层：

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

---

## 性能指标

| 指标 | 数值 |
|------|------|
| 回测吞吐 | > 1,000,000 events/sec |
| 撮合延迟（P99） | < 1 μs |
| RL 训练（8 env VecEnv） | > 10,000 steps/sec |
| 分布式加速（8 workers） | > 5x |
| 测试用例 | 1200+ Rust + 24 Python |

---

## 快速开始

```python
import axon_quant

env = axon_quant.rl.TradingEnv(
    config={"initial_capital": 100_000.0, "max_steps": 500},
    market_data=bars,
    action_space={"type": "continuous", "min": -1.0, "max": 1.0},
    reward="sharpe",
)

obs = env.reset()
obs, reward, terminated, truncated, info = env.step([0.5])
```

---

## 文档导航

- [安装与快速入门](getting-started/installation.md)
- [AI 原生核心设计](user-guide/ai-native-design.md)
- [策略研发全流程](user-guide/strategy-development.md)
- [LLM 智能体驱动交易](user-guide/llm-trading/oader.md)
- [生产部署与监控](user-guide/production.md)
- [传统策略迁移](user-guide/traditional-strategy.md)
- [模块参考（23 个 crate 详解）](user-guide/modules.md)
- [API 参考](reference/api-reference.md)
- [常见问题](about/faq.md)

---

## 免责声明

本项目是一个**开源量化交易框架**，仅供**研究和学习目的**使用。作者和贡献者**不对使用本软件造成的任何经济损失承担责任**。使用本软件即表示您理解并接受上述条款。详见 [LICENSE](https://github.com/pengwow/axon_quant/blob/main/LICENSE)。
