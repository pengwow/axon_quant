# 快速开始

> 适用版本:AXON v0.1.0+
> 前置阅读:[安装](installation.md)

本文档带你 5 分钟跑通 AXON 的第一个回测示例。

## 1. 跑通示例

仓库自带 6 个 RL 示例,先挑一个最直观的验证环境:

```bash
git clone https://github.com/pengwow/axon_quant.git
cd axon_quant

# 跑 L1 撮合回测示例(纯 Rust,无 Python 依赖)
cargo run -p axon-backtest --example simple_l1_backtest
```

预期输出:

```text
[INFO] axon-backtest 启动
[INFO] 加载市场数据: 1,000,000 ticks(50ms 粒度)
[INFO] 撮合引擎:L1(最优价成交)
[INFO] 模拟订单: 100 单
[INFO] 平均冲击: 2.3 bps
[INFO] 总收益: +12.4%
[INFO] Sharpe: 1.87
```

## 2. 第一个 Python 回测(可选)

```python
import axon_quant as aq
import numpy as np

# 1. 构造合成市场数据
n_ticks = 100_000
prices = 100 + np.cumsum(np.random.randn(n_ticks) * 0.01)
volumes = np.random.uniform(100, 1000, n_ticks)

# 2. 创建回测环境
env = aq.make_env(
    market_data=aq.MarketData.from_arrays(prices, volumes),
    matching_engine="L1",
    impact_model="almgren_chriss",
    latency_model="fixed_1ms",
    fee_model="taker_5bps",
)

# 3. 跑通一条 episode
obs = env.reset()
done = False
total_pnl = 0.0
while not done:
    # 简单策略:价格上涨买入 1 单位,下跌卖出
    action = 1 if env.current_price() > env.entry_price() else -1
    obs, reward, done, info = env.step(action)
    total_pnl += reward

print(f"Total PnL: {total_pnl:.2f}")
```

## 3. 跑通 LLM Trading 示例(Stage K 交付)

```bash
# Mock 后端 DryRun(无网络,验证工具链)
cargo test -p axon-llm --test llm_trading_mock_e2e

# Backtest 后端(对接 axon-backtest L1 撮合)
cargo test -p axon-llm --test llm_trading_backtest_e2e
```

详见 [LLM 交易架构](../user-guide/llm-trading/overview.md)。

## 下一步

- 🏗️ [架构总览](../user-guide/architecture.md) —— 了解系统组件和数据流
- 🤖 [LLM 交易架构](../user-guide/llm-trading/overview.md) —— 4 tool + 4 后端 + 3 风控
- 🛠️ [CLI 命令](../reference/cli.md) —— 用 CLI 跑回测 / 训练 / 优化
- 🐍 [Python 绑定](../reference/python-bindings.md) —— 完整 Python API
