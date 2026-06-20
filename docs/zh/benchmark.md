# 基准测试报告

本文档展示 AXON 量化交易框架的性能基准测试结果。

## 测试环境

- **操作系统**: macOS (Apple Silicon)
- **Rust 版本**: 1.96.0+
- **构建模式**: Release (optimized)

## 核心性能指标

| 指标 | 结果 | 说明 |
|------|------|------|
| Event Builder Tick | ~2.4 ns | 单个 Tick 事件构建延迟 |
| Event Builder Bar | ~2.2 ns | 单个 Bar 事件构建延迟 |
| Impact Linear | ~3.2 ns | 线性冲击模型计算 |
| Reward PnL | ~1.2 ns | PnL 奖励计算 |
| Reward Sharpe | ~111 ns | Sharpe 比率计算 |

## 详细报告

详细的基准测试报告可通过以下链接查看：

- [完整基准测试报告](./report/report/index.html)

## 运行基准测试

```bash
# 运行所有基准测试
make bench

# 运行单个 crate 的基准测试
cargo bench -p axon-core

# 运行特定基准测试
cargo bench -p axon-core -- event_builder_tick

# 生成报告到 docs 目录
make bench-report
```

## 基准测试说明

### axon-core

- **event_builder_tick**: 测试单个 Tick 事件的构建性能
- **event_builder_bar**: 测试单个 Bar 事件的构建性能
- **impact_linear**: 测试线性冲击模型的计算性能
- **reward_pnl**: 测试 PnL 奖励函数的计算性能
- **reward_sharpe**: 测试 Sharpe 比率奖励函数的计算性能

### axon-backtest

- **matching_latency**: 测试撮合引擎的延迟性能
- **order_book**: 测试订单簿操作的性能

### axon-rl

- **observation**: 测试观测空间构建的性能
- **trading_env**: 测试交易环境 step/reset 的性能
