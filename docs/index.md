# AXON Quant

AXON 是一个 **AI 原生**的量化交易框架，使用 Rust 编写高性能核心，通过 PyO3 提供 Python 绑定。

## 核心特性

- **强化学习原生**：内置 Gymnasium 兼容交易环境
- **LLM 智能体**：ReAct 推理循环 + Tool Calling
- **多后端推理**：ONNX / PyTorch / Candle
- **完整训练管线**：HPO + Walk-Forward + Tracker + Registry
- **可解释性内建**：KernelSHAP + 反事实解释
- **交易所直连**：Binance + OKX 合约 API

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

## 文档导航

- [AXON 是什么](user-guide/what-is-axon.md)
- [安装与快速入门](getting-started/installation.md)
- [AI 原生核心设计](user-guide/ai-native-design.md)
- [策略研发全流程](user-guide/strategy-development.md)
- [LLM 智能体驱动交易](user-guide/llm-trading/oader.md)
- [生产部署与监控](user-guide/production.md)
- [传统策略迁移](user-guide/traditional-strategy.md)
- [API 参考](reference/api-reference.md)
- [常见问题](about/faq.md)
