# API 参考

> **完整可运行示例**: [`examples/17_python_bindings/python_bindings_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/17_python_bindings/python_bindings_demo.py)
> 覆盖全部核心模块的 Python API 演示。

本文档提供 AXON 量化交易框架各顶层模块的速查表与关键 API 代码示例，帮助开发者快速定位所需功能。

---

## 1. 顶层模块速查表

| 模块 | Crate | 核心功能 | 主要类型 |
|------|-------|----------|----------|
| **rl** | `axon-rl` | Gymnasium 兼容的交易环境、动作/观测空间、奖励函数 | `TradingEnv`, `ActionSpace`, `ObservationSpace`, `RewardFn` |
| **llm** | `axon-llm` | LLM 后端抽象、ReAct Agent、工具调用 | `LLMBackend`, `ReActAgent`, `ToolDefinition`, `Message` |
| **hpo** | `axon-hpo` | 超参数优化（Optuna 集成）、Study / Trial 管理 | `HPOConfig`, `StudyConfig`, `TrialResult`, `SearchSpaceDef` |
| **walk_forward** | `axon-walk-forward` | 滚动/扩展窗口交叉验证、稳定性分析 | `WalkForwardConfig`, `FoldResult`, `AggregatedMetrics` |
| **tracker** | `axon-tracker` | 实验追踪（MLflow / 内存后端）、指标记录 | `ExperimentTracker`, `ParamValue`, `RunStatus` |
| **registry** | `axon-registry` | 模型版本管理、阶段转换、回滚 | `ModelRegistry`, `ModelVersion`, `ModelStage`, `SemVer` |
| **distributed** | `axon-distributed` | Ray 分布式训练、参数服务器、Checkpoint | `DistributedConfig`, `ClusterConfig`, `AlgorithmConfig` |
| **exchange** | `axon-exchange` | 交易所适配器、WebSocket、限流器 | `ExchangeAdapter`, `ExchangeConfig`, `RateLimitConfig` |
| **explain** | `axon-explain` | 可解释性：SHAP、反事实、报告生成 | `KernelSHAP`, `CounterfactualGenerator`, `ReportGenerator` |
| **ensemble** | `axon-ensemble` | 模型集成：投票、加权、动态权重、堆叠 | `DynamicWeightedEnsemble`, `EnsembleManager`, `StackingEnsemble` |
| **inference** | `axon-inference` | 模型推理引擎、热更新、多后端支持 | `InferenceEngine`, `ModelHotReloader`, `OnnxBackend`, `CandleBackend` |
| **backtest** | `axon-backtest` | 事件驱动回测引擎、撮合、冲击模型 | `BacktestEngine`, `MatchingEngine`, `RunResult` |

---

## 2. 关键 API 代码示例

### 2.1 TradingEnv — 交易环境

`TradingEnv` 是 AXON 的核心 RL 环境，完全兼容 Gymnasium 接口。

```python
from axon_quant import (
    TradingEnv, EnvConfig,
    DefaultObservationSpace, FeatureConfig, FeatureSource, NormalizerType,
    DiscreteActionSpace, TradingDirection,
    PnLReward, SharpeReward, MultiObjectiveReward,
    MarketBar,
)

# 1. 配置环境
config = EnvConfig(
    initial_capital=100_000.0,    # 初始资金 10 万 USDT
    transaction_cost=0.001,       # 交易成本 10 bps
    slippage=0.0005,              # 滑点 5 bps
    max_position_ratio=1.0,       # 最大满仓
    max_steps=1000,               # 每 episode 最大步数
    seed=None,                    # 随机种子
    symbol="BTCUSDT",             # 交易标的
    return_window=252,            # 收益率历史窗口（用于夏普计算）
)

# 2. 定义观测空间（特征工程）
obs_space = DefaultObservationSpace.new(
    window_size=20,               # 保留最近 20 个时间步
    features=[
        FeatureConfig(
            name="close",
            source=FeatureSource.PriceField("close"),
            normalizer=NormalizerType.ZScore,  # Z-Score 归一化
            clip_range=(-5.0, 5.0),            # 截断异常值
        ),
        FeatureConfig(
            name="volume",
            source=FeatureSource.VolumeField("volume"),
            normalizer=NormalizerType.ZScore,
        ),
        FeatureConfig(
            name="rsi",
            source=FeatureSource.RSI(14),      # 内置 RSI 计算
            normalizer=NormalizerType.MinMax,  # 映射到 [0, 1]
        ),
    ],
)

# 3. 定义动作空间（离散）
action_space = DiscreteActionSpace.new(
    n_quantity_bins=5,            # 5 个仓位档位: 20%/40%/60%/80%/100%
    direction=TradingDirection.Both,  # 允许做多和做空
)

# 4. 定义奖励函数（多目标）
reward_fn = MultiObjectiveReward([
    PnLReward(relative=True, scale=1.0),     # 相对收益率
    SharpeReward(risk_free_rate=0.02, window=20),  # 滚动夏普比率
])

# 5. 加载市场数据
market_data = load_bars("BTCUSDT", "1h", start="2024-01-01", end="2024-06-01")

# 6. 创建环境
env = TradingEnv.new(
    config=config,
    action_space=action_space,
    observation_space=obs_space,
    reward_fn=reward_fn,
    market_data=market_data,
)

# 7. 标准 Gymnasium 交互循环
obs = env.reset()
done = False
total_reward = 0.0

while not done:
    # 这里可以接入 RL 模型或规则策略
    action = model.predict(obs) if model else env.action_space.sample()
    
    obs, reward, done, info = env.step(action)
    total_reward += reward
    
    print(env.render())  # 输出: step=123/5000 | value=$102340.50 | pos=0.5000

print(f"Episode 总奖励: {total_reward:.2f}")
print(f"最终净值: {env.portfolio().portfolio_value:.2f}")
```

---

### 2.2 LLMBackend — LLM 后端

`LLMBackend` 是统一的 LLM 接口，支持 OpenAI、DeepSeek、本地推理服务等。

```python
from axon_quant import LLMBackend, Message, ToolDefinition, LLMResponse

# 创建 OpenAI 后端
llm = OpenAIBackend(
    api_key="YOUR_API_KEY",
    model="deepseek-chat",        # 或 "gpt-4", "claude-3-opus"
    base_url="https://api.deepseek.com",
)

# 基础对话
messages = [
    Message(role="system", content="你是一位专业的量化交易分析师。"),
    Message(role="user", content="分析 BTC 当前的技术面。"),
]

response = await llm.complete(messages)
print(response.content)

# Function Calling（工具调用）
tools = [
    ToolDefinition(
        name="get_price",
        description="获取指定交易对的当前价格",
        parameters={
            "type": "object",
            "properties": {
                "symbol": {"type": "string", "description": "交易对，如 BTCUSDT"},
            },
            "required": ["symbol"],
        },
    ),
    ToolDefinition(
        name="get_rsi",
        description="计算指定交易对的 RSI 指标",
        parameters={
            "type": "object",
            "properties": {
                "symbol": {"type": "string"},
                "period": {"type": "integer", "default": 14},
            },
            "required": ["symbol"],
        },
    ),
]

response = await llm.complete_with_tools(messages, tools)

# 解析工具调用
if response.tool_calls:
    for call in response.tool_calls:
        print(f"调用工具: {call.name}, 参数: {call.arguments}")
        # 执行工具并返回结果...

# 错误处理
from axon_quant import LLMError

try:
    response = await llm.complete(messages)
except LLMError.RateLimited as e:
    print(f"被限流，建议等待 {e.retry_after} 秒")
    await asyncio.sleep(e.retry_after or 60)
except LLMError.ContextOverflow as e:
    print(f"上下文超限: {e.needed} > {e.limit}")
    # 截断历史消息或切换长上下文模型
```

---

### 2.3 Tracker — 实验追踪

`ExperimentTracker` 提供统一的实验记录接口，支持 MLflow 和内存后端。

```python
from axon_quant import ExperimentTracker, MLflowTracker, MemoryTracker, ParamValue, RunStatus

# 创建 MLflow 追踪器（生产环境）
tracker = MLflowTracker(
    tracking_uri="http://localhost:5000",
    experiment_name="ppo_btc_trading",
    run_name="run_2024_06_18_v1",
)

# 或创建内存追踪器（测试/快速迭代）
# tracker = MemoryTracker.new()

# 记录超参数
tracker.log_param("learning_rate", ParamValue.Float(3e-4))
tracker.log_param("batch_size", ParamValue.Int(128))
tracker.log_param("hidden_size", ParamValue.Int(256))
tracker.log_param("env_symbol", ParamValue.String("BTCUSDT"))

# 批量记录参数
tracker.log_params([
    ("gamma", ParamValue.Float(0.99)),
    ("gae_lambda", ParamValue.Float(0.95)),
    ("clip_range", ParamValue.Float(0.2)),
])

# 记录指标（支持按 step 记录）
for step in range(1000):
    loss = train_step()
    tracker.log_metric("loss", loss, step=step)
    
    if step % 100 == 0:
        sharpe = evaluate_sharpe()
        tracker.log_metric("sharpe_ratio", sharpe, step=step)
        tracker.log_metric("portfolio_value", env.portfolio().portfolio_value, step=step)

# 记录直方图（如权重分布）
tracker.log_histogram("actor_weights", weights_flattened, step=1000)

# 记录图像（如收益曲线）
tracker.log_image("pnl_curve", png_bytes, format=ImageFormat.PNG, step=1000)

# 上传模型产物
tracker.log_artifact("model.onnx", Path("./models/model.onnx"))

# 设置标签
tracker.set_tag("model_type", "PPO")
tracker.set_tag("data_source", "binance_1h")

# 结束运行
tracker.finish(RunStatus.Success)

# 刷新缓冲区（确保数据已写入）
tracker.flush()
```

---

### 2.4 Registry — 模型注册表

`ModelRegistry` 管理模型的全生命周期：注册、阶段转换、回滚。

```python
from axon_quant import (
    ModelRegistry, LocalStorage,
    ModelMetadata, ModelStage, SemVer, ModelSignature,
    VersionFilter,
)
from pathlib import Path

# 创建注册表（本地文件存储）
storage = LocalStorage.new(base_dir="./model_registry")
registry = ModelRegistry.new(storage)

# 注册新模型版本
metadata = ModelMetadata(
    tags={
        "algorithm": "PPO",
        "env": "BTCUSDT_1h",
        "sharpe": "1.85",
    },
    description="PPO 模型 v3，优化了夏普比率",
)

signature = ModelSignature(
    inputs=["observation: float32[1,64,128]"],
    outputs=["action_probs: float32[1,3]"],
)

model_version = await registry.register(
    name="ppo_btc_trading",
    artifact_path=Path("./models/ppo_v3.onnx"),
    metadata=metadata,
    signature=signature,
)
print(f"注册成功: {model_version.name}@{model_version.version}")
# 输出: ppo_btc_trading@1.0.0

# 获取最新版本
latest = await registry.get("ppo_btc_trading", version=None)
print(f"最新版本: {latest.version}, 阶段: {latest.stage}")

# 获取 Production 版本
prod = await registry.get_production("ppo_btc_trading")

# 阶段转换: Staging -> Production
await registry.transition_stage(
    name="ppo_btc_trading",
    version=SemVer.parse("1.0.0"),
    new_stage=ModelStage.Production,
)
# 注意: 提升到 Production 时，旧 Production 版本自动降级为 Archived

# 查询版本列表
versions = await registry.list_versions(
    name="ppo_btc_trading",
    filter=VersionFilter(
        stage=ModelStage.Production,
        min_version=SemVer.parse("1.0.0"),
        limit=10,
    ),
)

# 回滚到上一个 Production 版本
rolled_back = await registry.rollback("ppo_btc_trading")
print(f"回滚到: {rolled_back.version}")

# 下载模型产物
await registry.download_artifact(
    name="ppo_btc_trading",
    version=SemVer.parse("1.0.0"),
    dest=Path("./downloads/ppo_v1.onnx"),
)

# 列出所有模型
models = registry.list_models()
print(f"已注册模型: {models}")
```

---

### 2.5 InferenceEngine — 推理引擎

`InferenceEngine` 提供统一的模型推理接口，支持 ONNX、tch、Candle 三后端。

```python
from axon_quant import (
    InferenceEngine, OnnxBackend, CandleBackend, TchBackend,
    ModelConfig, Device, InferenceBackend,
    Observation, Action,
)
from pathlib import Path

# 通用配置
config = ModelConfig(
    path="models/trading_model.onnx",
    backend=InferenceBackend.ONNX,
    device=Device.CUDA(0),        # 使用 GPU 0
    input_shape=[1, 64, 128],     # [batch, seq_len, features]
    output_dim=3,                 # Buy / Sell / Hold
    fp16=True,                    # 启用 FP16
    num_threads=4,                # CPU 线程数
)

# ONNX 后端
engine = OnnxBackend(config)
engine.load(Path(config.path))

# Candle 后端（纯 Rust，无 Python 依赖）
candle_config = ModelConfig(
    path="models/trading_model.safetensors",
    backend=InferenceBackend.CANDLE,
    device=Device.CPU,
    input_shape=[1, 4, 1],        # input_dim = 1*4*1 = 4
    output_dim=3,
    fp16=False,
    num_threads=4,
)
candle_engine = CandleBackend(candle_config)
candle_engine.load(Path(candle_config.path))

# 单条推理
obs = Observation(
    features=[0.5, -0.2, 1.1, 0.0, ...],  # 64*128=8192 维特征
    feature_names=[...],
    timestamp=1234567890,
)
action = engine.infer(obs)
print(f"预测动作: {action}")

# 批量推理（生产环境推荐）
observations = [obs1, obs2, obs3, obs4]
actions = engine.infer_batch(observations)
print(f"批量预测: {len(actions)} 个动作")

# 热更新（原子替换 session）
from axon_quant import ModelHotReloader

reloader = ModelHotReloader(engine, config)
reloader.spawn_watcher()  # 启动文件监控

# 手动触发重载
new_version = await reloader.reload()
print(f"模型已更新到版本 {new_version}")

# 订阅版本变化
version_rx = reloader.subscribe()
await version_rx.changed()
print(f"检测到新版本: {version_rx.borrow()}")
```

---

### 2.6 ExchangeAdapter — 交易所适配器

`ExchangeAdapter` 提供统一的交易所接口，目前支持 Binance 和 OKX。

```python
from axon_quant import (
    BinanceAdapter, OkxAdapter,
    ExchangeConfig, ExchangeId,
    RateLimitConfig, ReconnectConfig,
    MarginType, PositionMode,
)
from decimal import Decimal

# Binance 配置
config = ExchangeConfig(
    exchange_id=ExchangeId.Binance,
    api_key="YOUR_API_KEY",
    api_secret="YOUR_API_SECRET",
    passphrase=None,
    testnet=True,
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
    position_endpoint="/fapi/v2/positionRisk",
    fapi_base_url="https://testnet.binancefuture.com",
)

# 创建并连接
adapter = BinanceAdapter(config)
await adapter.connect()

# 订阅行情(0.6.0 Python 端仅接受字符串 symbol 列表,无 Symbol 类)
await adapter.subscribe(["BTCUSDT", "ETHUSDT"])

# 获取行情通道
market_rx = adapter.market_data_rx()
while True:
    msg = await market_rx.recv()
    match msg.type:
        case "Ticker":
            print(f"[{msg.data.symbol}] 买 {msg.data.bid} / 卖 {msg.data.ask}")
        case "Trade":
            print(f"成交: {msg.data.price} x {msg.data.quantity}")

# 下单(0.6.0 Python 端 place_order 仅接受 dict,非 Order 实例)
order = {
    "symbol": "BTCUSDT",
    "side": "buy",
    "type": "market",
    "quantity": "0.001",
    "tif": "GTC",
    "meta": {"strategy": "momentum_v1"},
}
order_id = await adapter.place_order(order)

# 撤单
await adapter.cancel_order(order_id)

# 合约操作
await adapter.set_leverage("BTCUSDT", leverage=10)
await adapter.set_margin_type("BTCUSDT", MarginType.Isolated)
await adapter.set_position_mode(hedge_mode=True)

# 查询账户
account = await adapter.get_account_info()
print(f"总余额: {account.total_balance}, 可用: {account.available_balance}")

# 查询资金费率
funding = await adapter.get_funding_rate("BTCUSDT")
print(f"资金费率: {funding.rate}, 下次结算: {funding.next_funding_ms}")
```

---

## 3. 配置参数参考表

### 3.1 EnvConfig（交易环境）

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `initial_capital` | `f64` | `100_000.0` | 初始资金 |
| `transaction_cost` | `f64` | `0.001` | 交易成本比例（10 bps） |
| `slippage` | `f64` | `0.0005` | 滑点比例（5 bps） |
| `max_position_ratio` | `f64` | `1.0` | 最大持仓比例（0.0 ~ 1.0） |
| `max_steps` | `usize` | `1000` | 每 episode 最大步数 |
| `seed` | `Option<u64>` | `None` | 随机种子 |
| `symbol` | `String` | `"BTCUSDT"` | 交易标的代码 |
| `return_window` | `usize` | `252` | 收益率历史窗口大小 |

### 3.2 ExchangeConfig（交易所）

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `exchange_id` | `ExchangeId` | - | 交易所标识（Binance / OKX） |
| `api_key` | `String` | - | API 密钥 |
| `api_secret` | `String` | - | API 密钥 |
| `passphrase` | `Option<String>` | `None` | OKX 专用 passphrase |
| `testnet` | `bool` | `true` | 是否使用测试网 |
| `rest_base_url` | `String` | - | REST API 基础 URL |
| `ws_url` | `String` | - | WebSocket URL |
| `rate_limit` | `RateLimitConfig` | - | 限流配置 |
| `reconnect` | `ReconnectConfig` | - | 重连配置 |
| `position_endpoint` | `String` | `"/fapi/v2/positionRisk"` | 持仓查询端点 |
| `fapi_base_url` | `Option<String>` | `None` | 合约 API 基础 URL |

### 3.3 HPOConfig（超参数优化）

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `study.study_name` | `String` | - | Study 名称 |
| `study.direction` | `StudyDirection` | `Maximize` | 优化方向 |
| `study.sampler` | `SamplerConfig` | `Tpe` | 采样器类型 |
| `study.pruner` | `PrunerConfig` | `MedianPruner` | 剪枝器类型 |
| `study.storage` | `Option<String>` | `None` | Optuna storage URL |
| `search_space` | `HashMap` | - | 参数搜索空间定义 |
| `hpo.n_trials` | `usize` | `50` | 总 trial 数 |
| `hpo.n_jobs` | `usize` | `1` | 并行 trial 数 |
| `hpo.timeout_seconds` | `Option<u64>` | `None` | 总超时 |
| `hpo.early_stopping` | `bool` | `false` | 是否启用早停 |

### 3.4 WalkForwardConfig（滚动验证）

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `train_size` | `usize` | - | 训练窗口大小 |
| `validation_size` | `usize` | `0` | 验证窗口大小 |
| `test_size` | `usize` | - | 测试窗口大小 |
| `step_size` | `usize` | - | 滚动步长 |
| `window_type` | `WindowType` | `Expanding` | 窗口类型（Rolling / Expanding） |
| `purge_gap` | `usize` | `0` | 训练-测试间防泄漏间隔 |
| `embargo_pct` | `f64` | `0.01` | Embargo 百分比 |

### 3.5 DistributedConfig（分布式训练）

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `cluster.num_workers` | `usize` | - | Worker 数量 |
| `cluster.num_cpus_per_worker` | `usize` | `1` | 每 Worker CPU 数 |
| `cluster.num_gpus_per_worker` | `f64` | `0.0` | 每 Worker GPU 数 |
| `cluster.cluster_address` | `Option<String>` | `None` | Ray 集群地址 |
| `algorithm.algorithm` | `String` | - | 算法名（PPO / SAC / DQN / IMPALA / APE_X） |
| `algorithm.framework` | `String` | `"torch"` | 框架（torch / tensorflow） |
| `resources.num_envs_per_worker` | `usize` | - | 每 Worker 环境数 |
| `resources.train_batch_size` | `usize` | - | 训练批大小 |
| `resources.sgd_minibatch_size` | `usize` | - | SGD minibatch 大小 |
| `fault_tolerance.checkpoint_interval_s` | `u64` | - | Checkpoint 间隔（秒） |
| `fault_tolerance.checkpoint_dir` | `String` | - | Checkpoint 保存目录 |

### 3.6 ModelConfig（推理引擎）

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `path` | `String` | - | 模型文件路径 |
| `backend` | `InferenceBackend` | - | 后端类型（ONNX / TCH / CANDLE） |
| `device` | `Device` | - | 设备（CPU / CUDA(n)） |
| `input_shape` | `[usize; 3]` | - | 输入形状 [batch, seq, features] |
| `output_dim` | `usize` | - | 输出维度 |
| `fp16` | `bool` | `false` | 是否启用 FP16 |
| `num_threads` | `usize` | `4` | CPU 推理线程数 |

---

## 4. 常用枚举速查

### 4.1 ActionSpace（动作空间）

```python
from axon_quant import ActionSpace, DiscreteActionSpace, ContinuousActionSpace, TradingDirection

# 离散动作空间
discrete = ActionSpace.Discrete(
    DiscreteActionSpace.new(n_quantity_bins=5, direction=TradingDirection.Both)
)
# 动作索引: 0=Hold, 1-5=Buy(20%-100%), 6-10=Sell(20%-100%)

# 连续动作空间
continuous = ActionSpace.Continuous(
    ContinuousActionSpace.new(min=-1.0, max=1.0)
)
# -1.0 = 满仓做空, 0.0 = 空仓, 1.0 = 满仓做多
```

### 4.2 NormalizerType（归一化策略）

```python
from axon_quant import NormalizerType

NormalizerType.ZScore    # (x - mean) / std，保留历史统计量
NormalizerType.MinMax    # (x - min) / (max - min) -> [0, 1]
NormalizerType.Robust    # (x - median) / IQR，抗异常值
NormalizerType.None      # 不归一化
```

### 4.3 ModelStage（模型阶段）

```python
from axon_quant import ModelStage

ModelStage.Staging      # 新注册，待验证
ModelStage.Production   # 线上运行
ModelStage.Archived     # 旧版本归档
ModelStage.RolledBack   # 已回滚
```

### 4.4 OrderType（订单类型）

```python
from axon_quant import OrderType

OrderType.Limit         # 限价单
OrderType.Market        # 市价单
OrderType.StopLoss      # 止损单
OrderType.StopLimit     # 限价止损单

# 注:`tif`(time-in-force) 在 0.6.0 收口时统一为 `tif` 字段,
# OMS `Order(symbol, ..., tif)` 接受 "GTC" / "IOC" / "FOK" 字符串字面量,
# backtest OrderDict `tif` 字段同步。Rust 端无独立 `TimeInForce` 枚举类。

---

## 5. 模块依赖关系

```text
                    ┌─────────────────┐
                    │   Application   │
                    └────────┬────────┘
                             │
        ┌────────────────────┼────────────────────┐
        │                    │                    │
        ▼                    ▼                    ▼
┌──────────────┐   ┌──────────────┐   ┌──────────────┐
│   backtest   │   │   exchange   │   │   ensemble   │
└──────────────┘   └──────────────┘   └──────────────┘
        │                    │                    │
        └────────────────────┼────────────────────┘
                             │
                    ┌────────┴────────┐
                    │      rl         │
                    │  (TradingEnv)   │
                    └────────┬────────┘
                             │
        ┌────────────────────┼────────────────────┐
        │                    │                    │
        ▼                    ▼                    ▼
┌──────────────┐   ┌──────────────┐   ┌──────────────┐
│  inference   │   │     llm      │   │   explain    │
└──────────────┘   └──────────────┘   └──────────────┘
                             │
                    ┌────────┴────────┐
                    │  core types     │
                    └─────────────────┘
```

---

## 6. 版本兼容性

AXON 当前版本为 `0.6.0`，各 crate 版本统一：

| Crate | 版本 | 最低 Rust 版本 |
|-------|------|---------------|
| axon-core | 0.6.0 | 1.96.0 |
| axon-rl | 0.6.0 | 1.96.0 |
| axon-llm | 0.6.0 | 1.96.0 |
| axon-inference | 0.6.0 | 1.96.0 |
| axon-exchange | 0.6.0 | 1.96.0 |
| axon-ensemble | 0.6.0 | 1.96.0 |
| axon-explain | 0.6.0 | 1.96.0 |
| axon-backtest | 0.6.0 | 1.96.0 |
| axon-hpo | 0.6.0 | 1.96.0 |
| axon-walk-forward | 0.6.0 | 1.96.0 |
| axon-tracker | 0.6.0 | 1.96.0 |
| axon-registry | 0.6.0 | 1.96.0 |
| axon-distributed | 0.6.0 | 1.96.0 |
| axon-monitor | 0.6.0 | 1.96.0 |
| axon-risk | 0.6.0 | 1.96.0 |
| axon-compliance | 0.6.0 | 1.96.0 |
