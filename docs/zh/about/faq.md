# 常见问题

> **相关示例**: [`examples/`](https://github.com/pengwow/axon_quant/blob/main/examples/) 目录包含所有功能的完整可运行示例。
> 推荐从 [`examples/01_getting_started/00_all_in_one.py`](https://github.com/pengwow/axon_quant/blob/main/examples/01_getting_started/00_all_in_one.py) 开始。

本文档汇总了 AXON 量化交易框架使用过程中的常见问题与解答，按类别组织以便快速定位。

---

## 1. 环境准备类

### Q1: AXON 的最低系统要求是什么？

**A:** AXON 框架的最低要求如下：

- **Rust 版本**: 1.97.0 或更高（使用 `rustc --version` 检查）
- **Python 版本**: 3.9 或更高（如需使用 Python 绑定）
- **操作系统**: Linux（推荐 Ubuntu 22.04+）、macOS 13+、Windows 11+
- **内存**: 最少 8GB，推荐 16GB（训练大规模模型时）
- **磁盘**: 最少 10GB 可用空间（含依赖和模型文件）

对于 GPU 推理：
- **NVIDIA GPU**: CUDA 11.8+ / cuDNN 8.6+
- **显存**: 最少 4GB，推荐 8GB+

```bash
# 检查 Rust 版本
rustc --version

# 检查 Python 版本
python3 --version

# 检查 CUDA（如使用 GPU）
nvidia-smi
```

---

### Q2: 如何安装 AXON 框架？

**A:** AXON 目前以 Rust workspace 形式组织，通过 Cargo 构建：

```bash
# 1. 克隆仓库
git clone https://github.com/your-org/axon_quant.git
cd axon_quant

# 2. 构建全部 crate（开发模式）
cargo build

# 3. 构建全部 crate（发布模式，推荐生产环境）
cargo build --release

# 4. 运行测试
cargo test --workspace

# 5. 构建特定 crate（如只需推理引擎）
cargo build -p axon-inference --release
```

如需 Python 绑定，需额外安装 `maturin`：

```bash
pip install maturin
maturin develop --release  # 开发安装
maturin build --release    # 构建 wheel
```

---

### Q3: 编译时遇到 ONNX Runtime 链接错误怎么办？

**A:** ONNX Runtime (`ort`) 依赖需要系统级库支持。常见解决方案：

**Linux (Ubuntu/Debian):**
```bash
sudo apt-get update
sudo apt-get install -y libgomp1 libssl-dev pkg-config
```

**macOS:**
```bash
brew install openssl
export OPENSSL_DIR=$(brew --prefix openssl)
```

**如果不需要 ONNX 后端**，可在 `Cargo.toml` 中禁用该 feature：

```toml
[dependencies]
axon-inference = { path = "../axon-inference", default-features = false, features = ["candle-backend"] }
```

---

### Q4: 如何配置开发环境以同时支持 Rust 和 Python？

**A:** 推荐以下开发环境配置：

```bash
# 1. 安装 Rust（如未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# 2. 安装 Python 依赖
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt  # 包含 maturin, numpy, pandas

# 3. 安装 pre-commit 钩子（代码格式化 + 检查）
pip install pre-commit
pre-commit install

# 4. 配置 IDE（VS Code 推荐插件）
# - rust-analyzer
# - Python
# - Even Better TOML
```

---

## 2. 使用问题类

### Q5: 如何创建自定义奖励函数？

**A:** 实现 `RewardFn` trait 即可自定义奖励：

```python
from axon_quant import RewardFn, PortfolioState, Action

class CustomReward(RewardFn):
    """
    自定义奖励函数：结合收益率、回撤惩罚和交易频率惩罚。
    """
    def __init__(self, drawdown_penalty: float = 0.5, trade_penalty: float = 0.01):
        self.drawdown_penalty = drawdown_penalty
        self.trade_penalty = trade_penalty
    
    def calculate(self, state, action, next_state, history):
        # 1. 基础收益率奖励
        pnl = next_state.portfolio_value - state.portfolio_value
        reward = pnl / state.portfolio_value if state.portfolio_value > 0 else 0
        
        # 2. 回撤惩罚（如果净值低于历史最高值）
        if history:
            peak = max(history)
            drawdown = (peak - next_state.portfolio_value) / peak
            reward -= self.drawdown_penalty * drawdown
        
        # 3. 交易频率惩罚（鼓励减少不必要的交易）
        if action.action_type != ActionType.Hold:
            reward -= self.trade_penalty
        
        return reward
    
    def name(self):
        return "custom_pnl_dd_trade"

# 使用自定义奖励函数
env = TradingEnv.new(
    config=config,
    action_space=action_space,
    observation_space=obs_space,
    reward_fn=CustomReward(drawdown_penalty=0.5, trade_penalty=0.01),
    market_data=market_data,
)
```

---

### Q6: TradingEnv 的 observation 包含哪些特征？

**A:** `Observation` 的结构取决于你配置的 `ObservationSpace`。默认的 `DefaultObservationSpace` 输出：

```python
observation = env.reset()

# observation.features: 一维浮点向量
# 形状 = [num_features * window_size]
# 例如: 3 个特征 x 20 个时间步 = 60 维向量

# observation.feature_names: 与 features 等长的特征名列表
# 例如: ["close_t0", "close_t1", ..., "volume_t0", "rsi_t0", ...]

# observation.timestamp: 当前时间戳（毫秒）
```

你可以通过 `obs_space.feature_names()` 查看完整的特征名列表：

```python
feature_names = obs_space.feature_names()
for i, name in enumerate(feature_names):
    print(f"{i}: {name} = {observation.features[i]:.4f}")
```

---

### Q7: 如何将训练好的模型导出为 ONNX 格式？

**A:** AXON 的推理引擎支持 ONNX，但模型导出取决于你的训练框架：

**PyTorch 模型导出：**
```python
import torch

# 假设 model 是你的 PyTorch 策略网络
dummy_input = torch.randn(1, 64, 128)  # 与模型输入形状一致

# 导出 ONNX
torch.onnx.export(
    model,
    dummy_input,
    "trading_model.onnx",
    input_names=["observation"],
    output_names=["action_probs"],
    dynamic_axes={
        "observation": {0: "batch_size"},
        "action_probs": {0: "batch_size"},
    },
    opset_version=14,
)
```

**导出后使用 AXON 加载：**
```python
from axon_quant import OnnxBackend, ModelConfig, InferenceBackend, Device

config = ModelConfig(
    path="trading_model.onnx",
    backend=InferenceBackend.ONNX,
    device=Device.CPU,
    input_shape=[1, 64, 128],
    output_dim=3,
)
engine = OnnxBackend(config)
engine.load(Path(config.path))
```

---

### Q8: 回测引擎如何处理滑点和交易成本？

**A:** 滑点和交易成本在 `EnvConfig` 中配置，由 `Executor` 在订单执行时自动扣除：

```python
config = EnvConfig(
    transaction_cost=0.001,  # 10 bps：每笔成交收取名义金额的 0.1%
    slippage=0.0005,         # 5 bps：成交价相对于信号价的偏移
    ...
)
```

**成本计算逻辑：**
- **交易成本**: `cost = filled_qty * fill_price * transaction_cost`
- **滑点**: `adjusted_price = fill_price * (1 + slippage * random_sign)`

这些成本会从 `portfolio_value` 中扣除，并反映在奖励函数中。

---

### Q9: 如何同时监控多个交易对的行情？

**A:** 使用 `ExchangeAdapter` 的批量订阅功能：

```python
# 订阅多个交易对
symbols = [
    Symbol("BTCUSDT"),
    Symbol("ETHUSDT"),
    Symbol("SOLUSDT"),
    Symbol("BNBUSDT"),
]
await adapter.subscribe(symbols)

# 在消息循环中区分交易对
market_rx = adapter.market_data_rx()
while True:
    msg = await market_rx.recv()
    symbol = msg.data.symbol
    
    if symbol == Symbol("BTCUSDT"):
        process_btc(msg.data)
    elif symbol == Symbol("ETHUSDT"):
        process_eth(msg.data)
```

注意：每个交易对会占用独立的 WebSocket 订阅槽位，需确保不超过交易所限制（Binance 默认每个连接最多 1024 个订阅）。

---

## 3. 性能问题类

### Q10: 推理延迟过高（> 1ms），如何优化？

**A:** 推理延迟优化的常见策略：

**1. 选择合适的后端**

| 后端 | 典型延迟 | 优化建议 |
|------|----------|----------|
| ONNX (CPU) | 200-500µs | 启用 `fp16`，设置 `num_threads=4` |
| ONNX (CUDA) | 50-200µs | 使用 TensorRT Execution Provider |
| Candle | 300-600µs | 纯 Rust，无 Python GIL 开销 |
| tch | 500-1000µs | 适合研究，生产环境建议转 ONNX |

**2. 启用批推理**
```python
# 单条推理（不推荐生产环境）
action = engine.infer(obs)  # ~500µs

# 批量推理（推荐）
actions = engine.infer_batch([obs1, obs2, obs3, obs4])  # ~600µs 总计
```

**3. 模型优化**
```python
# ONNX 模型优化（使用 onnxruntime-tools）
from onnxruntime.tools.optimizer import optimize_model

optimized = optimize_model(
    "model.onnx",
    model_type="bert",  # 或使用 "gpt2" 等
    use_gpu=True,
    opt_level=99,
)
optimized.save_model_to_file("model_optimized.onnx")
```

**4. 预热（Warmup）**
```python
# 首次推理通常较慢（内存分配、缓存预热）
# 生产启动时执行几次 dummy 推理
for _ in range(10):
    engine.infer(dummy_obs)
```

---

### Q11: 训练时内存占用过大，如何降低？

**A:** 内存优化的几个方向：

**1. 减小观测窗口**
```python
# 原配置：20 个时间步 x 50 个特征 = 1000 维
obs_space = DefaultObservationSpace.new(window_size=20, features=...)

# 优化后：10 个时间步 x 30 个特征 = 300 维
obs_space = DefaultObservationSpace.new(window_size=10, features=simplified_features)
```

**2. 使用 Rolling 窗口而非 Expanding**
```python
# Expanding 窗口会不断增长历史数据
# Rolling 窗口固定长度，内存更稳定
walk_forward_config = WalkForwardConfig.rolling(
    train_size=252, test_size=63, step_size=63
)
```

**3. 限制并行环境数**
```python
distributed_config = DistributedConfig(
    cluster=ClusterConfig.local(num_workers=4),
    resources=ResourceConfig(
        num_envs_per_worker=4,  # 从 8 减到 4
        ...
    ),
)
```

**4. 定期清理 Tracker 缓冲区**
```python
# 高频记录指标时，定期 flush 防止内存累积
for step in range(1_000_000):
    tracker.log_metric("reward", reward, step=step)
    if step % 1000 == 0:
        tracker.flush()  # 强制写入并释放缓冲区
```

---

### Q12: Walk-Forward 验证运行很慢，如何加速？

**A:** Walk-Forward 验证涉及多次训练+测试循环，可通过以下方式加速：

**1. 增大 step_size（减少 fold 数）**
```python
# 原配置：63 步一滚，产生大量 fold
config = WalkForwardConfig.expanding(train_size=252, test_size=63, step_size=63)

# 优化后：252 步一滚，fold 数减少 75%
config = WalkForwardConfig.expanding(train_size=252, test_size=63, step_size=252)
```

**2. 并行训练各 fold**
```python
import asyncio

async def train_fold(fold_data):
    # 每个 fold 独立训练
    model = train_model(fold_data.train)
    metrics = evaluate(model, fold_data.test)
    return metrics

# 并行执行所有 fold
fold_results = await asyncio.gather(*[train_fold(f) for f in folds])
```

**3. 使用更轻量的模型**
```python
# 减小网络规模
hpo_config = HPOConfig.new(
    study_name="fast_hpo",
    search_space={
        "hidden_size": SearchSpaceDef.choice([32, 64]),  # 从 [128, 256, 512] 减小
        "num_layers": SearchSpaceDef.choice([1, 2]),     # 从 [2, 3, 4] 减小
    },
    n_trials=20,  # 减少 trial 数
)
```

**4. 启用 HPO 剪枝**
```python
# 使用 MedianPruner 提前终止表现差的 trial
study_config = StudyConfig(
    study_name="ppo_fast",
    direction=StudyDirection.Maximize,
    pruner=PrunerConfig(
        pruner_type=PrunerType.MedianPruner(n_startup_trials=5)
    ),
)
```

---

## 4. 其他常见问题

### Q13: 如何处理交易所 API 限流？

**A:** AXON 内置 `TokenBucketRateLimiter` 自动处理限流：

```python
from axon_quant import TokenBucketRateLimiter

# 配置限流器
limiter = TokenBucketRateLimiter(
    requests_per_second=10,   # Binance 默认限制
    orders_per_minute=60,
    ws_messages_per_second=50,
)

# 在发送请求前获取令牌
limiter.try_acquire()  # 成功返回，失败抛出 RateLimited 异常

# ExchangeAdapter 内部已集成限流器，通常无需手动调用
# 如遇 429 错误，检查 config.rate_limit 是否配置正确
```

---

### Q14: 模型在生产环境出现 OOM（内存溢出）怎么办？

**A:** 生产环境 OOM 的常见原因和解决方案：

**原因 1: 观测历史无限增长**
```python
# 错误：未限制历史长度
self.price_history.append(bar.close)

# 正确：限制历史长度
MAX_HISTORY = 1000
self.price_history.append(bar.close)
if len(self.price_history) > MAX_HISTORY:
    self.price_history = self.price_history[-MAX_HISTORY:]
```

**原因 2: 模型热更新时旧模型未释放**
```python
# ModelHotReloader 已处理此问题
# 但自定义实现时需确保：
old_session = self.session
self.session = new_session
del old_session  # 显式释放
import gc
gc.collect()
```

**原因 3: 批推理时 batch size 过大**
```python
# 限制批大小
MAX_BATCH_SIZE = 32
for i in range(0, len(observations), MAX_BATCH_SIZE):
    batch = observations[i:i + MAX_BATCH_SIZE]
    actions = engine.infer_batch(batch)
```

---

### Q15: 如何调试 RL 智能体不学习的问题？

**A:** 智能体不学习的排查清单：

**1. 检查奖励函数**
```python
# 打印每步奖励
obs, reward, done, info = env.step(action)
print(f"Step {env.current_step()}: reward={reward:.4f}")

# 如果奖励始终为 0 或 NaN，检查 reward_fn 实现
```

**2. 检查观测值范围**
```python
# 确保观测值在合理范围（如 Z-Score 后应在 [-5, 5]）
obs = env.reset()
print(f"Obs min={min(obs.features):.2f}, max={max(obs.features):.2f}")

# 如果出现极大值，检查 normalizer 配置
```

**3. 检查动作解码**
```python
# 确保动作被正确解码为订单
action = Action.discrete(1)  # Buy 20%
order = decoder.decode(action, portfolio)
print(f"Decoded order: {order}")

# 如果 order 为 None，检查 action 是否合法（如现金不足）
```

**4. 使用 Tracker 记录训练曲线**
```python
# 记录关键指标
tracker.log_metric("episode_reward", total_reward, step=episode)
tracker.log_metric("portfolio_value", env.portfolio().portfolio_value, step=episode)
tracker.log_metric("num_trades", env.info().trades_executed, step=episode)

# 在 MLflow UI 中查看曲线是否单调上升
```

**5. 简化环境验证**
```python
# 先用固定策略测试环境是否正常
class BuyAndHold:
    def predict(self, obs):
        return Action.discrete(1)  # 始终买入

# 如果 BuyAndHold 能盈利，说明环境正常，问题在 RL 算法
```

---

## 5. 快速索引

| 问题关键词 | 对应 Q&A |
|-----------|---------|
| 安装 / 编译 / 系统要求 | Q1, Q2, Q3, Q4 |
| 奖励函数 / Observation / 模型导出 | Q5, Q6, Q7 |
| 滑点 / 成本 / 多品种监控 | Q8, Q9 |
| 推理延迟 / 内存 / 训练速度 | Q10, Q11, Q12 |
| 限流 / OOM / 不学习 | Q13, Q14, Q15 |

如以上解答未能解决你的问题，建议：
1. 查看对应模块的 Rust 源码文档（`cargo doc --open`）
2. 运行模块级单元测试（`cargo test -p <crate-name>`）
3. 在 GitHub Issues 中提交问题，附上最小复现代码
