# LLM Trading Example

基于 LLM 的量化交易示例，展示如何使用 ReAct Agent 进行自动交易。

## 功能特性

- **合成数据生成**: 随机游走 K 线数据，可复现的价格序列
- **ReAct Agent**: 基于 ReAct 模式的交易决策 Agent
- **Rule-Based Mock Provider**: SMA/RSI 规则引擎，模拟 LLM 交易决策
- **交易工具**: 下单、查询持仓、市场数据、风险检查
- **轨迹记录**: 完整的交易轨迹记录与 JSON Schema 验证
- **50-bar 交易循环**: 标准化的交易回测流程

## 文件结构

```
examples/llm_trading/
├── run_50bar.py          # 50-bar 交易主循环
├── mock_provider.py      # Rule-Based Mock Provider
├── trajectory.schema.json # Trajectory JSON Schema
└── README.md             # 本文件
```

## 快速开始

### 运行 50-bar 交易循环

```bash
python examples/llm_trading/run_50bar.py --seed 42
```

### 参数说明

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| --seed | int | 42 | 随机种子 |
| --output | str | output | 输出目录 |
| --instrument | str | BTC-USDT | 交易品种 |

## 使用 Rule-Based Mock Provider

```python
from examples.llm_trading.mock_provider import RuleBasedMockProvider

provider = RuleBasedMockProvider(seed=42, sma_short=5, sma_long=20)

# 更新价格历史
provider.update_price(50000.0)

# 生成交易决策
response = provider.generate_response("")
print(response["thought"])
print(response["action"])
```

### 规则引擎策略

1. **SMA 金叉**: 短期 SMA 上穿长期 SMA，且 RSI < 70 → 买入
2. **SMA 死叉**: 短期 SMA 下穿长期 SMA，且 RSI > 30 → 卖出
3. **RSI 超买**: RSI > 75 → 卖出
4. **RSI 超卖**: RSI < 25 → 买入
5. **噪声**: 20% 概率随机决策

## Trajectory 数据结构

交易轨迹遵循 `trajectory.schema.json` 定义的 JSON Schema：

```json
{
  "version": "0.10.0",
  "run_id": "run-42",
  "instrument": "BTC-USDT",
  "provider": "mock",
  "model": "mock-model",
  "seed": 42,
  "bars": [
    {
      "bar_id": 0,
      "ts": 1700000000000,
      "thought": "市场上涨趋势，买入",
      "action": {...},
      "observation": "Bought 0.05 BTC @ 50200.00",
      "reward": 15.50,
      "cum_pnl": 15.50
    }
  ],
  "summary": {
    "total_pnl": 123.45,
    "trades": 15,
    "final_position": 0.5,
    "wall_time_s": 0.123,
    "cost_usd": 0.0
  }
}
```

## 使用 Python Agent 模块

```python
from axon_quant.agent import ReActAgent, TradingTools, TrajectoryRecorder
from axon_quant.trading import MockTradingBackend

backend = MockTradingBackend()
tools = TradingTools(backend)

recorder = TrajectoryRecorder(
    run_id="run-42",
    instrument="BTC-USDT",
    provider="mock",
    model="mock-model",
    seed=42,
)

def llm_provider(prompt):
    return """Thought: 市场上涨趋势
Action: {"tool": "place_order", "args": {"symbol": "BTC-USDT", "side": "Buy", "quantity": 0.1, "price": 50000.0}}
Observation: Order placed"""

agent = ReActAgent(
    llm_provider=llm_provider,
    tools=tools.to_tool_list(),
    trajectory_recorder=recorder,
)

history = []
result = agent.run_step(history, "Current price: 50000")
print(result)
```

## 测试

```bash
# 运行所有测试
make python-build && pytest tests/python/test_run_50bar_mock.py -v

# 运行特定测试
pytest tests/python/test_run_50bar_mock.py::TestRuleBasedMockProvider -v
```

## 输出示例

```
Trajectory saved to output/trajectory_42.json
Final PnL: 81.56
Total trades: 31
```

## 依赖

- Python 3.10+
- pytest
- jsonschema

## License

MIT