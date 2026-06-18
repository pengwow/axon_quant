# AI 原生核心设计

> AXON 不是"传统量化框架 + AI 插件"，而是从数据管道到生产部署为 AI 工作流重新设计的统一框架。本章深入解析其四大设计支柱、模块联动机制与统一数据管道。

---

## 什么是 AI 原生

"AI 原生"（AI-Native）不是营销词汇，而是 AXON 在架构层面的四项根本设计决策：

### 1. 统一数据管道：训练与生产共用同一套 Arrow 列式数据

传统量化系统中，研究员用 Pandas DataFrame 做特征工程，工程师用 Protobuf / 自建格式写实盘，两者之间的"格式转换层"是 bug 与信息损失的高发区。

AXON 的 `axon-data` 从底层采用 **Apache Arrow `RecordBatch`** 作为唯一内存表示：

```rust
// axon-data/src/pipeline.rs
// FeaturePipeline 对 Dataset 执行 fit + transform，全程列式零拷贝

pub trait Normalizer: Send + Sync {
    /// 训练阶段：从 dataset 学到归一化参数
    fn fit(&mut self, ds: &Dataset);

    /// 推理阶段：把 dataset 转为 FeatureMatrix
    fn transform(&self, ds: &Dataset) -> FeatureMatrix;
}

/// Z-Score 归一化器：(x - mean) / std
pub struct ZScoreNormalizer {
    mean: f64,
    std: f64,
}
```

- **训练时**：`fit_transform()` 从历史数据学到均值/方差，输出 `FeatureMatrix` 供神经网络消费
- **生产时**：同一 `transform()` 路径，使用训练阶段保存的 `mean` / `std`，保证分布一致
- **零拷贝**：Arrow 列式 buffer 直接透传到底层 SIMD 归一化，避免 `Vec<Tick>` 中间表示

!!! note "为什么选 Arrow"
    Arrow 的列式内存布局与零拷贝特性，使得 Rust 内核、Python 训练脚本、ONNX 推理引擎可以共享同一块内存，无需序列化/反序列化开销。

### 2. 训练与生产同一套代码：TradingEnv 底层即回测引擎

在传统框架中，回测用一套 Python 脚本，实盘用另一套 C++ 服务，策略逻辑需要"翻译"两次。

AXON 的 `TradingEnv` 直接包装 `axon-backtest` 的撮合引擎：

```rust
// axon-rl/src/env/trading_env.rs
// TradingEnv::step() 内部调用 Executor::execute()，与回测引擎共用同一套订单簿逻辑

pub fn step(&mut self, action: &Action) -> EnvResult<StepResult> {
    // 1. 动作 → 订单（ActionDecoder 统一解析离散/连续动作）
    let order = self.decoder.decode(action, &self.portfolio)?;

    // 2. 执行订单 → 底层即 Backtest 撮合引擎（含冲击模型与滑点）
    if let Some(o) = order {
        let results = self.executor.execute(&[o], &current_bar, &mut self.portfolio)?;
        for r in &results {
            if r.filled {
                self.trades_executed += 1;
                self.transaction_costs += r.cost;
            }
        }
    }

    // 3. 按下一根 K 线 close 重估组合市值
    self.executor.revalue(&mut self.portfolio, next_bar.close)?;

    // 4. 计算奖励（PnL / Sharpe / Sortino 共用同一套 ReturnHistory）
    let reward = self.reward_fn.calculate(...)?;

    Ok((obs, reward, self.done, info))
}
```

**上实盘时，只需替换 `ExchangeAdapter`**：

```rust
// axon-exchange/src/traits.rs
/// 统一交易所接口：Binance / OKX 均实现此 trait
pub trait ExchangeAdapter: Send + Sync {
    async fn place_order(&self, req: OrderRequest) -> Result<OrderAck, ExchangeError>;
    async fn cancel_order(&self, id: &OrderId) -> Result<(), ExchangeError>;
    async fn query_portfolio(&self) -> Result<PortfolioSnapshot, ExchangeError>;
    async fn subscribe_market_data(&self, symbols: &[Symbol]) -> Result<DataStream, ExchangeError>;
}
```

策略代码（`reset` / `step` / `render`）完全不变，实现**训练与生产零差异**。

### 3. LLM 与 RL 互补："直觉引擎" + "推理引擎"双模决策

AXON 同时内置两种 AI 范式，并通过 `axon-ensemble` 实现协同：

| 能力 | RL（`axon-rl`） | LLM（`axon-llm`） |
|------|----------------|------------------|
| **决策方式** | 模式识别 + 统计优化 | 符号推理 + 自然语言理解 |
| **优势场景** | 高频微观结构、价格预测 | 宏观事件解读、财报分析、异常检测 |
| **输入** | 归一化特征向量 | 文本上下文（新闻、公告、链上数据） |
| **输出** | 连续仓位 / 离散动作 | 结构化工具调用（下单 / 查询 / 分析） |
| **可解释性** | 需 SHAP 事后归因 | 推理链（Chain-of-Thought）天然可解释 |

```rust
// axon-llm/src/trading/mod.rs
// LLM 交易工具：place_order / query_portfolio，带 SafetyMode 风控

pub use place_order_tool::PlaceOrderTool;
pub use query_portfolio_tool::QueryPortfolioTool;
pub use safety::{DailyCounter, RiskLimits, SafetyMode};
```

`axon-ensemble` 的 `DynamicWeightedEnsemble` 可实时监控 RL 与 LLM 子模型的夏普比率，动态调整权重：

```rust
// axon-ensemble/src/dynamic.rs
// 在线性能监控 + 自动权重调整

pub struct DynamicWeightedEnsemble {
    models: Vec<Box<dyn Policy>>,
    weights: Vec<f64>,
    performance_window: VecDeque<f64>,
}

impl Ensemble for DynamicWeightedEnsemble {
    fn update_weights(&mut self, performances: &[f64]) {
        // 根据近期夏普比率衰减低性能模型权重
        // ...
    }
}
```

### 4. 可解释性内建：SHAP + 反事实 + 决策报告，非事后补丁

传统框架中，可解释性是"训练完再想办法"的附加任务。AXON 将 `Explainer` 定义为与 `Policy` 同等级别的核心 trait：

```rust
// axon-explain/src/traits.rs
/// Explainer trait：为一次模型决策生成完整解释
pub trait Explainer: Send + Sync {
    /// 解释一次完整决策（特征归因 + 反事实 + 注意力可视化）
    fn explain(
        &self,
        observation: &HashMap<String, f64>,
        action: &ActionSnapshot,
    ) -> Result<Explanation, ExplainabilityError>;

    /// 生成反事实解释："如果当时不买入，收益会如何变化"
    fn generate_counterfactuals(
        &self,
        observation: &HashMap<String, f64>,
        action: &ActionSnapshot,
        max_changes: usize,
    ) -> Vec<CounterfactualExplanation>;
}
```

每次 `step()` 产生的决策可同步生成 `ExplanationReport`，随模型版本一同归档到 `axon-registry`，满足合规审计要求。

---

## 模块联动矩阵

AXON 的 6 大 AI 核心模块并非孤立存在，它们通过 trait 与共享类型形成紧密联动：

| | **RL** | **LLM** | **Inference** | **Explain** | **Ensemble** | **Exchange** |
|:---|:---|:---|:---|:---|:---|:---|
| **RL** | — | LLM 作为 `Policy` 接入 `Ensemble` | `InferenceEngine` 为 `TradingEnv` 提供模型预测 | `Explainer` 解释 `TradingEnv` 的每步决策 | `VecEnv` 并行 rollout 供 `Ensemble` 评估 | `TradingEnv` 底层撮合逻辑与 `ExchangeAdapter` 对齐 |
| **LLM** | RL 策略作为 Tool 供 LLM 调用 | — | `InferenceEngine` 加速 LLM 后端（本地模型） | `explain` 模块为 LLM 决策生成归因 | LLM 输出通过 `Ensemble` 与 RL 策略融合 | `PlaceOrderTool` / `QueryPortfolioTool` 直接调用 `ExchangeAdapter` |
| **Inference** | 为 RL `Policy` 提供低延迟推理 | 为 LLM 提供本地模型（Candle）推理 | — | `Explainer` 需要 `ModelPredictor` 评估反事实输入 | `Ensemble` 聚合多个 `InferenceEngine` 输出 | 生产环境 `InferenceEngine` 通过 `ExchangeAdapter` 获取实时行情 |
| **Explain** | 解释 RL 策略的每步动作 | 解释 LLM 的工具调用决策 | 解释模型预测的特征重要性 | — | 为 `Ensemble` 中各子模型生成独立解释报告 | 为交易所异常行为（如滑点突变）生成归因 |
| **Ensemble** | 集成多个 RL 策略（PPO / SAC / 规则） | 集成多个 LLM 后端（OpenAI / 本地） | 集成 ONNX / Candle / tch 三后端输出 | 聚合多模型解释，生成一致性报告 | — | `Ensemble` 的最终动作通过 `ExchangeAdapter` 下单 |
| **Exchange** | 回测数据喂给 `TradingEnv` | 实时行情喂给 LLM 上下文 | 实时特征喂给 `InferenceEngine` | 成交记录用于验证解释准确性 | 成交结果反馈给 `Ensemble` 更新权重 | — |

### 联动示例：LLM 感知宏观事件 → RL 调整仓位

```
┌─────────────┐     财报文本      ┌─────────────┐
│  外部新闻源  │ ───────────────→ │   axon-llm  │
│ (API/爬虫)   │                  │  (ReAct 推理)│
└─────────────┘                  └──────┬──────┘
                                        │ "建议减仓 30%"
                                        ▼
                              ┌─────────────────────┐
                              │   axon-ensemble      │
                              │ (DynamicWeighted)    │
                              │  RL权重 0.7 → 0.5    │
                              │  LLM权重 0.3 → 0.5   │
                              └──────────┬──────────┘
                                         │ 融合动作
                                         ▼
                              ┌─────────────────────┐
                              │  axon-inference      │
                              │ (ONNX 推理 < 500µs)  │
                              └──────────┬──────────┘
                                         │ 目标仓位
                                         ▼
                              ┌─────────────────────┐
                              │  axon-exchange       │
                              │ (Binance/OKX 下单)   │
                              └─────────────────────┘
```

---

## 统一数据管道图

AXON 的所有模块共享同一套数据流，从源头到消费端无格式转换断层：

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              数据源层 (DataSource)                           │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐      │
│  │ CSV 文件  │  │ Parquet  │  │ WebSocket│  │  Mock    │  │ 交易所 API│      │
│  │ (本地)    │  │ (列式)   │  │ (实时流) │  │ (合成)   │  │ (REST)   │      │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘      │
│       └─────────────┴─────────────┴─────────────┴─────────────┘              │
│                                     │                                        │
│                                     ▼                                        │
│                    ┌────────────────────────────┐                            │
│                    │    axon-data (统一接入)      │                            │
│                    │  - schema 校验               │                            │
│                    │  - 时间对齐 / 去重            │                            │
│                    │  - 列式缓存 (mmap)           │                            │
│                    └─────────────┬──────────────┘                            │
│                                  │                                          │
│                                  ▼                                          │
│                    ┌────────────────────────────┐                            │
│                    │   Arrow RecordBatch (内存)  │                            │
│                    │  ┌────────────────────────┐ │                            │
│                    │  │ timestamp │ open │ ... │ │  ← 零拷贝，多语言共享        │
│                    │  └────────────────────────┘ │                            │
│                    └─────────────┬──────────────┘                            │
│                                  │                                          │
└──────────────────────────────────┼──────────────────────────────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                              消费层 (Consumers)                              │
│                                                                             │
│   ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐        │
│   │   TradingEnv     │    │  FeaturePipeline │    │  BacktestEngine │        │
│   │  (axon-rl)       │    │  (axon-data)     │    │  (axon-backtest)│        │
│   │                  │    │                  │    │                 │        │
│   │  reset() ──→ obs │    │  fit() / transform│   │  L1/L2/L3 撮合  │        │
│   │  step()  ──→ (o,r,d,i)│  → FeatureMatrix │    │  + 冲击模型      │        │
│   └────────┬────────┘    └────────┬────────┘    └─────────────────┘        │
│            │                      │                                         │
│            │                      ▼                                         │
│            │           ┌─────────────────┐                                 │
│            │           │ InferenceEngine │                                 │
│            │           │ (axon-inference)│                                 │
│            │           │                 │                                 │
│            │           │ ONNX / Candle   │                                 │
│            │           │ 批推理 < 1ms    │                                 │
│            │           └────────┬────────┘                                 │
│            │                    │                                          │
│            └────────────────────┼──────────────────────────────────────────┘
│                                 │
│                                 ▼
│                    ┌────────────────────────────┐
│                    │      ExchangeAdapter        │
│                    │    (axon-exchange)          │
│                    │  Binance / OKX 实盘下单      │
│                    └────────────────────────────┘
│
└─────────────────────────────────────────────────────────────────────────────┘
```

### 数据流关键节点说明

1. **DataSource**：`axon-data` 的 `DataSource` trait 统一封装 CSV / Parquet / WebSocket / Mock / 交易所 API 五种数据源。新增数据源只需实现 `fetch(&self, req: DataRequest) -> Result<Dataset, DataError>`。

2. **Arrow RecordBatch**：所有数据源最终转换为 Arrow 列式格式。`Dataset::iter_batches()` 返回 `&RecordBatch`，下游模块通过 `downcast_ref::<Float64Array>()` 直接读取列数据，无中间结构体分配。

3. **TradingEnv**：从 `RecordBatch` 中提取 `MarketBar`（OHLCV），按 `EnvConfig` 初始化组合状态。`step()` 内部将动作解码为订单，调用 `Executor` 撮合，更新 `PortfolioState`。

4. **FeaturePipeline**：对 `Dataset` 执行 `fit_transform()`，输出 `FeatureMatrix`（`Vec<f32>` 行优先）。该矩阵可直接喂给 ONNX / Candle 推理引擎，或作为 `Observation` 进入 `TradingEnv`。

5. **InferenceEngine**：接收 `Observation`（含 `features: Vec<f32>`），通过 `BatchInferencePipeline` 异步批处理，返回 `Action`。CPU 亲和性模块自动绑核，降低跨核 cache miss。

6. **ExchangeAdapter**：生产环境下，`Ensemble` 的最终动作通过 `ExchangeAdapter::place_order()` 提交到 Binance / OKX。回测环境下，同一动作通过 `BacktestEngine::execute()` 在本地撮合。

---

## AI 原生价值总结

| 传统痛点 | AXON 的 AI 原生方案 | 对应模块 |
|---------|-------------------|---------|
| 训练/生产数据格式不一致 | Arrow `RecordBatch` 统一列式存储，零拷贝透传 | `axon-data` |
| 回测引擎与实盘引擎两套代码 | `TradingEnv` 底层即回测撮合，上实盘仅替换 `ExchangeAdapter` | `axon-rl` + `axon-backtest` + `axon-exchange` |
| 模型训练完无法解释 | `Explainer` trait 内建，每步决策同步生成 SHAP + 反事实报告 | `axon-explain` |
| 单模型鲁棒性差 | `Ensemble` 支持 5 种集成策略，在线监控自动调权 | `axon-ensemble` |
| 超参优化与主项目松耦合 | `axon-hpo` 原生集成 Optuna + NSGA-II，直接操作 `TradingEnv` | `axon-hpo` |
| 模型部署需手动导出 + 封装服务 | `axon-inference` 三后端 + 热更新 + 批推理，开箱即用 | `axon-inference` |
| LLM 与量化系统割裂 | `axon-llm` ReAct 智能体内置交易工具，通过 `Ensemble` 与 RL 协同 | `axon-llm` + `axon-ensemble` |

!!! tip "核心理念"
    AXON 的 AI 原生设计并非追求"用最前沿的模型"，而是追求"让最前沿的模型能顺畅地融入量化交易全链路"。数据一致、代码一致、解释一致、部署一致 —— 这是 AXON 与传统框架的本质区别。

---

## 下一步

- [AXON 是什么](what-is-axon.md) — 回顾 AXON 的整体定位与核心特征
- [安装与快速入门](../getting-started/installation.md) — 动手安装并运行第一个随机策略基线
- 阅读 `examples/02_rl_training/train_ppo.py` — 体验 RL 策略训练完整流程
- 阅读 `crates/axon-llm/examples/integrated_trading_demo.rs` — 体验 LLM + RL 集成交易
