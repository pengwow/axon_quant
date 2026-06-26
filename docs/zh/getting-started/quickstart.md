# 快速开始

> 适用版本:AXON v0.2.0+
> 前置阅读:[安装](installation.md)

本文档带你 5 分钟跑通 AXON 的第一个回测示例。

## 1. 跑通示例

仓库自带多个示例,先挑一个最直观的验证环境:

```bash
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# 运行随机策略基线（纯 Python，无需外部依赖）
PYTHONPATH=examples .venv/bin/python examples/02_rl_training/random_agent.py
```

预期输出:

```text
[random_agent] 运行 5 个随机 episode，每个最多 500 步
=== 随机策略基线 ===
  episodes        : 5
  mean_reward     : -0.1234
  mean_steps      : 500.0
  mean_final_value: 98765.43
  elapsed         : 0.15s
PASS: 随机策略运行正常
```

## 2. 第一个 Python 回测（可选）

```python
import axon_quant

# 1. 构造合成市场数据
data = [
    {"timestamp": i, "open": 100.0, "high": 100.5, "low": 99.5,
     "close": 100.0, "volume": 1000.0}
    for i in range(500)
]

# 2. 创建回测引擎
from axon_quant.backtest import L1MatchingEngine, limit_order

engine = L1MatchingEngine()

# 3. 提交订单
result = engine.submit(limit_order(1, "BTCUSDT", "Buy", 100.0, 1.0))
print(f"Order filled: {result['is_filled']}, Fills: {len(result['fills'])}")
```

## 3. RL 训练示例

```bash
# 安装 RL 依赖
pip install axon_quant[rl]

# 运行 PPO 训练
PYTHONPATH=examples .venv/bin/python examples/02_rl_training/train_ppo.py \
    --timesteps 5000
```

## 下一步

- 🏗️ [架构总览](../user-guide/architecture.md) —— 了解系统组件和数据流
- 🤖 [LLM 交易架构](../user-guide/llm-trading/overview.md) —— 4 tool + 4 后端 + 3 风控
- 🛠️ [CLI 命令](../reference/cli.md) —— 用 CLI 跑回测 / 训练 / 优化
- 🐍 [Python 绑定](../reference/python-bindings.md) —— 完整 Python API
