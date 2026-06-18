# 场景三 — 生产部署与监控

本文档涵盖 AXON 量化交易框架从实验室走向生产环境的全流程，包括推理后端选型、模型热更新、可解释性审计、模型集成动态权重，以及交易所对接的完整实现。

---

## 1. 推理引擎三后端选择指南

AXON 的推理引擎 (`axon-inference`) 支持三种后端，分别适用于不同的部署场景。

### 1.1 后端对比表

| 维度 | ONNX | tch (PyTorch C++) | Candle (纯 Rust) |
|------|------|-------------------|------------------|
| **依赖** | `ort` (ONNX Runtime) | `tch-rs` (LibTorch) | `candle-core` + `candle-nn` |
| **二进制体积** | 中等 (+ ONNX Runtime) | 大 (+ LibTorch) | 小 (纯 Rust) |
| **启动速度** | 快 | 中等 | 极快 |
| **CPU 推理延迟** | < 500µs | < 1ms | < 500µs |
| **GPU 支持** | CUDA / TensorRT | CUDA / ROCm | CUDA (实验性) |
| **模型格式** | `.onnx` | `.pt` / `.torchscript` | `.safetensors` |
| **热更新支持** | `replace_session` | `replace_session` | `load(new_path)` |
| **适用场景** | 生产环境首选 | 研究/快速迭代 | 无 Python 依赖的精简部署 |

### 1.2 后端选择决策树

```text
是否需要 GPU 加速?
├── 是 → 需要 TensorRT?
│   ├── 是 → ONNX (Level3 优化 + TensorRT EP)
│   └── 否 → tch (CUDA) 或 ONNX (CUDA EP)
└── 否 → 是否介意 Python 依赖?
    ├── 是 → Candle (纯 Rust, 零 Python)
    └── 否 → ONNX (CPU, 生态最成熟)
```

### 1.3 配置示例

```python
from axon_quant import InferenceBackend, Device, ModelConfig

# ONNX 生产配置
onnx_config = ModelConfig(
    path="models/production.onnx",
    backend=InferenceBackend.ONNX,
    device=Device.CUDA(0),          # 使用第一块 GPU
    input_shape=[1, 64, 128],       # [batch, seq_len, features]
    output_dim=3,                   # Buy / Sell / Hold
    fp16=True,                      # 启用 FP16 推理
    num_threads=4,                  # ONNX Runtime 线程数
)

# Candle 无依赖配置
candle_config = ModelConfig(
    path="models/production.safetensors",
    backend=InferenceBackend.CANDLE,
    device=Device.CPU,
    input_shape=[1, 4, 1],          # input_dim = 1*4*1 = 4
    output_dim=3,
    fp16=False,                     # Candle 暂不支持 FP16
    num_threads=4,
)
```

---

## 2. 模型热更新

生产环境中，模型需要在不重启服务的情况下完成更新。AXON 通过 `ModelHotReloader` + `notify` 文件监控实现原子替换。

### 2.1 核心机制

```text
文件系统监控 (notify)
       │
       ▼
检测到模型文件变更
       │
       ▼
防抖处理 (500ms 聚合连续事件)
       │
       ▼
计算新模型 SHA256 校验
       │
       ▼
获取 backend 写锁 → 加载新模型 → 释放写锁
       │
       ▼
原子递增版本号 → 通过 watch channel 广播
```

### 2.2 热更新代码示例

```python
import asyncio
from axon_quant import ModelHotReloader, OnnxBackend, ModelConfig

async def setup_hot_reload():
    """
    配置模型热更新系统。
    
    工作流程:
    1. 创建推理后端 (OnnxBackend)
    2. 用 ModelConfig 包装后端,创建 ModelHotReloader
    3. 启动文件监控 (spawn_watcher)
    4. 订阅版本号变化,在回调中切换线上流量
    """
    config = ModelConfig(
        path="/data/models/trading_model.onnx",
        backend="onnx",
        device="cuda:0",
        input_shape=[1, 64, 128],
        output_dim=3,
        fp16=True,
        num_threads=4,
    )
    
    # 创建 ONNX 后端并加载初始模型
    backend = OnnxBackend(config)
    backend.load(config.path)
    
    # 创建热更新器: 包装 backend + config
    reloader = ModelHotReloader(backend, config)
    
    # 启动文件监控 (notify 库监控模型文件所在目录)
    # 当检测到 Modify/Create 事件时,自动触发 reload
    watcher_handle = reloader.spawn_watcher()
    
    # 订阅版本号变化 (tokio::sync::watch)
    version_rx = reloader.subscribe()
    
    # 在独立任务中监听版本变化
    async def on_version_change():
        while True:
            # 等待版本号更新
            await version_rx.changed()
            new_version = version_rx.borrow()
            print(f"[HotReload] 模型已更新到版本 {new_version}")
            # 这里可以执行: 切换负载均衡权重、刷新缓存等
    
    asyncio.create_task(on_version_change())
    return reloader, watcher_handle

# 也可以手动触发热更新 (如通过管理 API)
async def manual_reload(reloader: ModelHotReloader):
    """手动触发模型重载,返回新版本号。"""
    try:
        new_version = await reloader.reload()
        print(f"手动重载成功,新版本: {new_version}")
        return new_version
    except Exception as e:
        print(f"重载失败,保留当前版本: {e}")
        raise
```

### 2.3 关键实现细节 (Rust)

`ModelHotReloader` 的核心 Rust 实现位于 `crates/axon-inference/src/hot_reload.rs`：

- **`reload()` 方法**：独占写锁 (`RwLock::write()`) 加载新模型，保证替换期间无并发读取
- **`spawn_watcher()`**：使用 `notify::RecommendedWatcher` 监控目录，`tokio::sync::mpsc` 做事件去抖
- **版本广播**：通过 `tokio::sync::watch::channel` 向所有订阅者原子推送新版本号
- **SHA256 校验**：每次加载后计算模型文件哈希，写入日志便于审计

---

## 3. 可解释性审计

生产系统必须能够解释每一次交易决策的原因。AXON 通过 `axon-explain` + `axon-llm` 提供完整的可解释性链路。

### 3.1 核心组件

| 组件 | 功能 | 源码位置 |
|------|------|----------|
| `KernelSHAP` | 特征归因：量化每个特征对决策的贡献 | `crates/axon-explain/src/shap.rs` |
| `CounterfactualGenerator` | 反事实解释："如果某个特征改变,决策会如何变化" | `crates/axon-explain/src/counterfactual.rs` |
| `DecisionRecorder` | 决策记录器：fire-and-forget 异步记录 | `crates/axon-llm/src/explain/recorder.rs` |
| `ExplanationStore` | 解释存储：线程安全的 FIFO 缓存 | `crates/axon-llm/src/explain/store.rs` |
| `ReportGenerator` | 报告生成：HTML / Markdown 格式 | `crates/axon-explain/src/report.rs` |

### 3.2 KernelSHAP 特征归因

```python
from axon_quant import KernelSHAP, ModelPredictor
import numpy as np

class MyModelPredictor(ModelPredictor):
    """
    包装已训练模型,实现 ModelPredictor trait。
    predict() 接收特征向量,返回各动作维度的预测分数。
    """
    def __init__(self, model):
        self.model = model
    
    def predict(self, features: list[float]) -> list[float]:
        # 调用实际模型推理
        return self.model.infer(features)

# 背景数据：用于构建 SHAP 的基准分布
# 通常取训练集的特征均值或随机样本
background = np.random.randn(100, 20).tolist()  # 100 条样本,20 维特征

# 创建 KernelSHAP 解释器
shap_explainer = KernelSHAP(
    model=MyModelPredictor(trading_model),
    background=background,
    n_samples=256,  # 采样联盟数量,越大越精确但越慢
)

# 对单次观测进行解释
observation = [0.5, -0.2, 1.1, 0.0, ...]  # 20 维特征
shap_values = shap_explainer.compute_shap(observation)

# shap_values[i] 表示第 i 个特征对该次预测的贡献
for i, val in enumerate(shap_values):
    print(f"特征 {i}: SHAP = {val:+.4f}")
```

### 3.3 反事实解释

```python
from axon_quant import CounterfactualGenerator, CounterfactualConfig

# 配置反事实生成器
cf_config = CounterfactualConfig(
    max_changes=3,           # 最多修改 3 个特征
    step_size=0.5,           # 向背景均值移动 50%
    confidence_threshold=0.05,  # 置信度变化超过 5% 才保留
)

cf_generator = CounterfactualGenerator.with_feature_names(
    model=MyModelPredictor(trading_model),
    feature_names=[
        "rsi_14", "macd", "volume_ratio", "price_change_1h",
        "funding_rate", "open_interest", ...
    ],
    config=cf_config,
)

# 生成反事实解释
counterfactuals = cf_generator.generate(
    observation={
        "rsi_14": 75.0,
        "macd": 0.05,
        "volume_ratio": 2.5,
        ...
    },
    action=current_action_snapshot,
    explainer=shap_explainer,
)

for cf in counterfactuals:
    print(f"反事实: {cf.narrative}")
    # 输出示例:
    # "如果 rsi_14 从 75.00 变为 62.50, 置信度将从 85.00% 变为 78.00%"
```

### 3.4 决策记录与审计

```python
from axon_quant import DecisionRecorder, ExplainerBridge, ExplanationStore
import asyncio

# 创建解释存储 (容量 1000, FIFO 淘汰)
store = ExplanationStore.new(capacity=1000)

# 创建 ExplainerBridge (连接 LLM 后端与解释存储)
bridge = ExplainerBridge(
    llm_backend=llm_backend,      # OpenAI/DeepSeek 等 LLM 后端
    explainer=shap_explainer,     # SHAP 解释器
    store=store,
)

# 创建决策记录器 (fire-and-forget 异步)
recorder = DecisionRecorder(bridge)

# 每次交易决策后,异步记录解释
# 不阻塞主交易循环
recorder.record_async(DecisionRecord(
    decision_id="trade_2024_001",
    timestamp=time.time(),
    observation=observation,
    action=action,
    model_version=reloader.version(),
))

# 后续查询解释
async def query_explanation(decision_id: str):
    """查询某次决策的完整解释。"""
    explanation = await store.get(decision_id)
    if explanation:
        print(f"摘要: {explanation.summary}")
        print(f"置信度: {explanation.confidence:.2%}")
        for feat in explanation.top_features:
            print(f"  - {feat.name}: {feat.shap_value:+.4f}")
    return explanation
```

### 3.5 生成审计报告

```python
from axon_quant import ReportGenerator
from datetime import datetime, timezone

# 聚合一段时间内的所有解释
explanations = await store.latest(n=100)

# 生成决策报告
report = ReportGenerator.generate_decision_report(
    report_id="daily_audit_2024_06_18",
    explanations=explanations,
    period_start=datetime(2024, 6, 18, 0, 0, tzinfo=timezone.utc),
    period_end=datetime(2024, 6, 18, 23, 59, tzinfo=timezone.utc),
)

# 导出 HTML 报告 (含 CSS 样式、特征重要性条形图)
html = report.html_content
with open("audit_report.html", "w") as f:
    f.write(html)

# 导出 Markdown 报告 (适合版本控制)
markdown = report.markdown_content
with open("audit_report.md", "w") as f:
    f.write(markdown)
```

---

## 4. 模型集成动态权重

单一模型在复杂市场环境中容易失效。AXON 的 `axon-ensemble` 提供多种集成策略，其中 `DynamicWeightedEnsemble` 根据模型近期表现动态调整权重。

### 4.1 三种集成策略对比

| 策略 | 机制 | 适用场景 |
|------|------|----------|
| **HardVote** | 多数表决，每个模型一票 | 模型数量多、差异大 |
| **SoftVote** | 概率平均，综合各模型置信度 | 模型置信度可靠 |
| **WeightedVote** | 按固定权重加权平均 | 有先验模型优劣知识 |
| **DynamicWeighted** | 根据 Sharpe 比率动态调整权重 | 生产环境首选 |
| **Stacking** | 元学习器组合基模型预测 | 有充足训练数据 |

### 4.2 DynamicWeightedEnsemble 完整代码

```python
from axon_quant import (
    DynamicWeightedEnsemble, EnsembleManager,
    HardVoteStrategy, SoftVoteStrategy, WeightedVoteStrategy,
    Policy, Observation, Action, ModelPerformance,
)
from typing import List
import time

class MomentumPolicy(Policy):
    """示例策略：基于动量的规则策略。"""
    def __init__(self, name: str, lookback: int = 10):
        self._name = name
        self.lookback = lookback
    
    def name(self) -> str:
        return self._name
    
    def model_type(self):
        return ModelType.RuleBased
    
    def predict(self, observation: Observation) -> Action:
        # 简化示例：根据价格动量决定买卖
        prices = observation.market_features[-self.lookback:]
        momentum = (prices[-1] - prices[0]) / prices[0] if prices[0] != 0 else 0
        
        if momentum > 0.02:
            return Action(action_type=ActionType.Buy, confidence=abs(momentum))
        elif momentum < -0.02:
            return Action(action_type=ActionType.Sell, confidence=abs(momentum))
        else:
            return Action(action_type=ActionType.Hold, confidence=0.5)
    
    def action_probs(self, observation: Observation):
        action = self.predict(observation)
        if action.action_type == ActionType.Buy:
            return ActionProbabilities(buy=action.confidence, sell=0.0, hold=0.1)
        elif action.action_type == ActionType.Sell:
            return ActionProbabilities(buy=0.0, sell=action.confidence, hold=0.1)
        else:
            return ActionProbabilities(buy=0.0, sell=0.0, hold=action.confidence)

class RLPolicy(Policy):
    """示例策略：包装 RL 模型。"""
    def __init__(self, name: str, model_path: str):
        self._name = name
        self.model = load_rl_model(model_path)
    
    def name(self) -> str:
        return self._name
    
    def model_type(self):
        return ModelType.PPO
    
    def predict(self, observation: Observation) -> Action:
        return self.model.predict(observation)
    
    def action_probs(self, observation: Observation):
        return self.model.action_probs(observation)

# ==================== 集成策略使用 ====================

def create_dynamic_ensemble() -> DynamicWeightedEnsemble:
    """
    创建动态加权集成。
    
    权重计算公式:
        score_i = sharpe_i - penalty * |drawdown_i|
        weight_i = max(0, score_i) / sum(max(0, score_j))
    
    无历史表现时退化为均匀权重。
    """
    models: List[Policy] = [
        MomentumPolicy("momentum_short", lookback=5),
        MomentumPolicy("momentum_long", lookback=20),
        RLPolicy("ppo_v1", "models/ppo_v1.pt"),
        RLPolicy("sac_v1", "models/sac_v1.pt"),
    ]
    
    ensemble = DynamicWeightedEnsemble(
        models=models,
        decay_factor=0.95,       # 历史权重衰减因子 (预留)
        volatility_penalty=2.0,   # 回撤惩罚系数：回撤越大权重越低
    )
    return ensemble

# 模拟交易循环中更新模型表现
def update_model_performance(ensemble: DynamicWeightedEnsemble):
    """
    每日收盘后,根据当日表现更新各模型权重。
    """
    now = int(time.time())
    
    # 假设从某处获取各模型当日表现
    performances = [
        ModelPerformance(
            model_name="momentum_short",
            accuracy=0.55,
            sharpe_ratio=1.2,
            max_drawdown=-0.03,
            total_return=0.01,
            sample_count=10,
            last_evaluated=now,
        ),
        ModelPerformance(
            model_name="momentum_long",
            accuracy=0.48,
            sharpe_ratio=0.8,
            max_drawdown=-0.05,
            total_return=-0.005,
            sample_count=10,
            last_evaluated=now,
        ),
        ModelPerformance(
            model_name="ppo_v1",
            accuracy=0.62,
            sharpe_ratio=1.8,
            max_drawdown=-0.02,
            total_return=0.015,
            sample_count=10,
            last_evaluated=now,
        ),
        ModelPerformance(
            model_name="sac_v1",
            accuracy=0.58,
            sharpe_ratio=1.5,
            max_drawdown=-0.04,
            total_return=0.012,
            sample_count=10,
            last_evaluated=now,
        ),
    ]
    
    for perf in performances:
        ensemble.update_performance(perf)
    
    # 查看当前权重
    weights = ensemble.get_weights()
    for w in weights:
        print(f"模型 {w.model_name}: 权重 = {w.weight:.4f}")
    # 预期输出: ppo_v1 权重最高, momentum_long 可能因负收益被置零

# 使用 EnsembleManager 进行统一管理
def create_managed_ensemble():
    """
    EnsembleManager 提供统一预测接口 + 多样性度量 + 历史记录。
    """
    # 使用软投票策略
    strategy = SoftVoteStrategy()
    manager = EnsembleManager(strategy)
    
    # 注册模型
    manager.register_model(MomentumPolicy("mom_5", 5))
    manager.register_model(MomentumPolicy("mom_20", 20))
    manager.register_model(RLPolicy("ppo", "ppo.pt"))
    
    # 预测
    obs = Observation(market_features=[...], ...)
    action = manager.predict(obs, timestamp=int(time.time()))
    
    # 计算模型多样性 (0.0 = 完全一致, 1.0 = 完全分歧)
    diversity = manager.compute_diversity([obs])
    print(f"模型多样性: {diversity:.2%}")
    
    return manager
```

### 4.3 Stacking 集成 (高级)

```python
from axon_quant import StackingEnsemble, MetaModel

# 创建元模型 (线性层 + softmax)
# n_features = n_models * 3 (buy/sell/hold 概率) + n_models (置信度) + 32 (观测特征)
meta_model = MetaModel.new(
    n_features=3 * 4 + 4 + 32,  # 4 个基模型
    n_actions=3,                # Buy / Sell / Hold
)

# 加载预训练元模型权重
meta_model = MetaModel.with_weights(weights, bias)

# 创建堆叠集成
stacking = StackingEnsemble(
    base_models=[model1, model2, model3, model4],
    meta_model=meta_model,
)

# 预测：基模型预测 → 构造堆叠特征 → 元模型推理
action = stacking.predict(observation)
```

---

## 5. 交易所对接完整流程

AXON 通过 `axon-exchange` 提供统一的交易所适配器，目前支持 Binance 和 OKX。

### 5.1 架构概览

```text
┌─────────────────────────────────────────────────────────────┐
│                      Trading Service                        │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │   Strategy  │  │  RiskEngine │  │  ComplianceModule   │  │
│  └──────┬──────┘  └──────┬──────┘  └──────────┬──────────┘  │
│         │                │                     │             │
│         └────────────────┼─────────────────────┘             │
│                          ▼                                   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │              ExchangeAdapter (trait)                 │   │
│  │  connect / disconnect / subscribe / send_order ...   │   │
│  └────────────────────────┬─────────────────────────────┘   │
│                           │                                  │
│         ┌─────────────────┼─────────────────┐               │
│         ▼                 ▼                 ▼               │
│  ┌────────────┐   ┌────────────┐   ┌────────────┐          │
│  │  Binance   │   │    OKX     │   │  (Future)  │          │
│  │  Adapter   │   │  Adapter   │   │  Adapter   │          │
│  └────────────┘   └────────────┘   └────────────┘          │
└─────────────────────────────────────────────────────────────┘
```

### 5.2 Binance 适配器完整代码

```python
import asyncio
from axon_quant import (
    BinanceAdapter, ExchangeConfig, ExchangeId,
    Symbol, Order, OrderId, OrderType, Side, TimeInForce,
    RateLimitConfig, ReconnectConfig,
)
from decimal import Decimal

async def setup_binance() -> BinanceAdapter:
    """
    初始化 Binance 适配器 (支持现货 + 合约)。
    
    包含功能:
    - REST API 连接 (带 HMAC-SHA256 签名)
    - WebSocket 连接 (自动重连 + 指数退避)
    - 订单簿 / Ticker / K线 / 成交 实时推送
    - 限流器 (Token Bucket)
    - 合约功能: 杠杆 / 保证金模式 / 持仓模式 / 资金费率
    """
    config = ExchangeConfig(
        exchange_id=ExchangeId.Binance,
        api_key="YOUR_API_KEY",
        api_secret="YOUR_API_SECRET",
        passphrase=None,  # Binance 不需要 passphrase
        testnet=True,     # 使用测试网
        rest_base_url="https://testnet.binance.vision",
        ws_url="wss://testnet.binance.vision/ws",
        rate_limit=RateLimitConfig(
            requests_per_second=10,
            orders_per_minute=60,
            ws_messages_per_second=50,
        ),
        reconnect=ReconnectConfig(
            max_retries=10,
            initial_backoff_ms=500,
            max_backoff_ms=30000,
            backoff_multiplier=2.0,
            circuit_breaker_threshold=5,
            circuit_breaker_reset_sec=60,
        ),
        position_endpoint="/fapi/v2/positionRisk",  # 合约持仓端点
        fapi_base_url="https://testnet.binancefuture.com",
    )
    
    adapter = BinanceAdapter(config)
    
    # 连接: 验证 REST ping + 启动 WebSocket (含重连监督任务)
    await adapter.connect()
    print("Binance 连接成功")
    
    # 订阅行情 (深度、Ticker、成交、K线)
    await adapter.subscribe([
        Symbol("BTCUSDT"),
        Symbol("ETHUSDT"),
    ])
    print("行情订阅成功")
    
    return adapter

async def trading_loop(adapter: BinanceAdapter):
    """
    主交易循环: 接收 WebSocket 推送,执行策略,发送订单。
    """
    # 获取行情接收通道
    market_rx = adapter.market_data_rx()
    
    while True:
        msg = await market_rx.recv()
        
        match msg.type:
            case "Ticker":
                # 更新最新价格
                ticker = msg.data
                print(f"[{ticker.symbol}] 买 {ticker.bid} / 卖 {ticker.ask}")
            
            case "Depth":
                # 更新订单簿缓存
                depth = msg.data
                print(f"[{depth.symbol}] 买单 {len(depth.bids)} 层 / 卖单 {len(depth.asks)} 层")
            
            case "Trade":
                # 成交推送
                trade = msg.data
                print(f"[{trade.symbol}] 成交: {trade.price} x {trade.quantity}")
            
            case "OrderUpdate":
                # 订单状态更新
                update = msg.data
                print(f"订单 {update.client_order_id} 状态: {update.status}")
            
            case _:
                pass

async def place_order(adapter: BinanceAdapter):
    """
    下单示例: 市价买入 0.001 BTC。
    """
    order = Order(
        client_order_id=OrderId.new(),  # UUID v7
        symbol=Symbol("BTCUSDT"),
        side=Side.Buy,
        order_type=OrderType.Market,
        price=None,                    # 市价单不需要价格
        quantity=Decimal("0.001"),
        time_in_force=TimeInForce.Gtc,
        exchange=ExchangeId.Binance,
        meta={"strategy": "momentum_v1"},
    )
    
    order_id = await adapter.send_order(order)
    print(f"订单已发送,客户端 ID: {order_id}")
    return order_id

async def cancel_order(adapter: BinanceAdapter, order_id: OrderId):
    """撤单: Binance 需要 symbol + clientOrderId。"""
    await adapter.cancel_order(order_id)
    print(f"订单 {order_id} 已撤销")

async def futures_operations(adapter: BinanceAdapter):
    """
    合约操作示例 (Binance USDⓈ-M)。
    """
    symbol = "BTCUSDT"
    
    # 设置杠杆 (1-125x)
    await adapter.set_leverage(symbol, leverage=10)
    print(f"杠杆设置为 10x")
    
    # 设置保证金模式 (逐仓 / 全仓)
    await adapter.set_margin_type(symbol, MarginType.Isolated)
    print("保证金模式: 逐仓")
    
    # 设置持仓模式 (对冲 / 单向)
    await adapter.set_position_mode(hedge_mode=True)
    print("持仓模式: 对冲")
    
    # 查询杠杆分层
    brackets = await adapter.get_leverage_brackets(symbol)
    for b in brackets:
        print(f"层级 {b.bracket}: 最大杠杆 {b.max_leverage}x, "
              f"名义上限 {b.max_notional}, 维持保证金率 {b.maint_margin_ratio}")
    
    # 查询资金费率 (永续合约 8h 结算)
    funding = await adapter.get_funding_rate(symbol)
    print(f"资金费率: {funding.rate}, 下次结算: {funding.next_funding_ms}")
    
    # 查询账户信息
    account = await adapter.get_account_info()
    print(f"总余额: {account.total_balance}, 可用: {account.available_balance}")
    
    # 查询持仓量 (市场情绪)
    oi = await adapter.get_open_interest(symbol)
    print(f"持仓量: {oi.contracts} 张")
    
    # 查询多空比
    ratio = await adapter.get_long_short_ratio(symbol)
    print(f"多空比: {ratio.long_short_ratio:.2f} (多 {ratio.long_ratio:.1%} / 空 {ratio.short_ratio:.1%})")
```

### 5.3 OKX 适配器

OKX 适配器的接口与 Binance 完全一致，仅需更换配置：

```python
from axon_quant import OkxAdapter

async def setup_okx() -> OkxAdapter:
    """初始化 OKX 适配器。"""
    config = ExchangeConfig(
        exchange_id=ExchangeId.Okx,
        api_key="YOUR_OKX_API_KEY",
        api_secret="YOUR_OKX_API_SECRET",
        passphrase="YOUR_PASSPHRASE",  # OKX 需要 passphrase
        testnet=True,
        rest_base_url="https://www.okx.com",
        ws_url="wss://wspap.okx.com:8443/ws/v5/public?brokerId=9999",
        rate_limit=RateLimitConfig(...),
        reconnect=ReconnectConfig(...),
        position_endpoint="/api/v5/account/positions",
        fapi_base_url=None,  # OKX 合约与现货共享 rest_base_url
    )
    
    adapter = OkxAdapter(config)
    await adapter.connect()
    return adapter
```

### 5.4 限流器

```python
from axon_quant import TokenBucketRateLimiter

# 创建限流器: 每秒 10 个请求
limiter = TokenBucketRateLimiter(requests_per_second=10)

# 尝试获取令牌
for i in range(15):
    try:
        limiter.try_acquire()
        print(f"请求 {i+1}: 通过")
    except RateLimited as e:
        print(f"请求 {i+1}: 被限流,需等待 {e.wait_ms}ms")
        await asyncio.sleep(e.wait_ms / 1000)
```

### 5.5 WebSocket 管理器

```python
from axon_quant import WebSocketManager

# WebSocket 管理器提供连接状态 + 熔断器 + 退避计算
manager = WebSocketManager(reconnect_config)

# 连接成功回调
manager.on_connect_success()
print(f"连接状态: {manager.is_connected()}")

# 连接失败时自动计数,超过阈值触发熔断
for _ in range(6):
    manager.on_connect_failure()
print(f"熔断器状态: {manager.is_circuit_open()}")  # True

# 计算第 n 次重连的退避时长
backoff = manager.calculate_backoff(attempt=3)
print(f"第 3 次重连退避: {backoff:.1f}s")
```

---

## 6. 生产环境检查清单

在将策略部署到生产环境前，请逐项确认以下事项。

### 6.1 基础设施

- [ ] **硬件资源**：CPU / GPU / 内存满足推理延迟要求 (< 500µs)
- [ ] **网络**：交易所 API 服务器延迟 < 50ms，有备用网络链路
- [ ] **时钟同步**：NTP 同步，所有服务器时间误差 < 10ms
- [ ] **磁盘**：模型文件、日志、审计数据有充足存储空间

### 6.2 模型与推理

- [ ] **模型验证**：回测夏普比率 > 1.0，最大回撤 < 10%
- [ ] **热更新测试**：模拟模型更新，验证无停机、无推理错误
- [ ] **后端选择**：ONNX (生产) / Candle (无 Python 依赖) / tch (研究)
- [ ] **批推理**：多资产并发时延迟 < 1ms
- [ ] **FP16 验证**：如启用 FP16，验证精度损失在可接受范围

### 6.3 风控与合规

- [ ] **风控引擎**：`axon-risk` 已启用，包含订单大小、仓位、杠杆、回撤检查
- [ ] **熔断器**：连续亏损 N 笔后自动暂停，冷却期后恢复
- [ ] **合规审计**：`axon-compliance` 已配置，所有交易记录不可篡改
- [ ] **大额交易告警**：超过阈值自动触发监管报送

### 6.4 交易所对接

- [ ] **API 密钥**：使用子账户 + IP 白名单，最小权限原则
- [ ] **测试网验证**：所有功能在测试网通过至少 1 周模拟交易
- [ ] **限流配置**：requests_per_second / orders_per_minute 符合交易所限制
- [ ] **WebSocket 重连**：断线后自动重连 + 重新订阅，消息不丢失
- [ ] **订单状态跟踪**：所有订单从发送到成交/撤销的全生命周期可追踪

### 6.5 监控与告警

- [ ] **指标采集**：`axon-monitor` 已注册 Counter / Gauge / Histogram
- [ ] **延迟监控**：P99 推理延迟、P99 订单往返延迟
- [ ] **告警规则**：延迟 > 10ms、错误率 > 0.1%、断线次数 > 5 次/分钟
- [ ] **健康检查**：Kubernetes liveness / readiness 探针已配置
- [ ] **日志聚合**：结构化日志 (JSON) 已接入 ELK / Loki

### 6.6 可解释性与审计

- [ ] **SHAP 解释**：每次交易决策都有特征归因记录
- [ ] **反事实记录**：关键决策的反事实解释已存档
- [ ] **决策记录器**：`DecisionRecorder` 已接入，fire-and-forget 不阻塞交易
- [ ] **报告生成**：日报 / 月报 / 年报自动生成并归档

### 6.7 灾备与回滚

- [ ] **模型版本管理**：`ModelRegistry` 已配置，支持秒级回滚
- [ ] **Checkpoint**：分布式训练 checkpoint 定期保存
- [ ] **数据库备份**：交易记录、审计日志每日备份
- [ ] **应急预案**：交易所 API 故障、模型失效、网络中断的 SOP

---

## 7. 总结

生产部署是量化交易系统的最后一公里。AXON 通过以下设计保障生产可靠性：

1. **多后端推理**：ONNX / tch / Candle 灵活选型，满足不同部署要求
2. **原子热更新**：notify 文件监控 + 写锁替换 + 版本广播，零停机更新
3. **可解释审计**：KernelSHAP + 反事实 + 决策记录器，满足监管要求
4. **动态集成**：DynamicWeightedEnsemble 根据市场自适应调整模型权重
5. **交易所适配器**：Binance / OKX 统一接口，内置重连、限流、熔断

建议在生产上线前，先在测试网运行至少 2 周，验证全链路稳定性后再逐步放量。
