# API 文档

AXON 的完整 Rust API 文档由 `cargo doc` 自动生成并发布到 **docs.rs**:

- [axon-core](https://docs.rs/axon-core)
- [axon-backtest](https://docs.rs/axon-backtest)
- [axon-rl](https://docs.rs/axon-rl)
- [axon-hpo](https://docs.rs/axon-hpo)
- [axon-walk-forward](https://docs.rs/axon-walk-forward)
- [axon-distributed](https://docs.rs/axon-distributed)
- [axon-tracker](https://docs.rs/axon-tracker)
- [axon-registry](https://docs.rs/axon-registry)
- [axon-llm](https://docs.rs/axon-llm)
- [axon-cli](https://docs.rs/axon-cli)
- [axon-data](https://docs.rs/axon-data)
- [axon-compliance](https://docs.rs/axon-compliance)
- [axon-explain](https://docs.rs/axon-explain)
- [axon-ensemble](https://docs.rs/axon-ensemble)
- [axon-integration-tests](https://docs.rs/axon-integration-tests)

## 本地生成

```bash
cargo doc --workspace --no-deps --open
```

输出在 `target/doc/` 目录。

## 关键类型速查

### axon-core

- `Order` / `OrderId` / `OrderSide` / `OrderType` / `TimeInForce`
- `MarketData` / `Tick` / `Bar` / `OrderBook`
- `Event` / `EventQueue` / `SimulatedClock`
- `Portfolio` / `Position` / `Cash`
- `Scheduler` / `ImpactModel` / `LatencyModel` / `FeeModel`

### axon-backtest

- `L1MatchingEngine` / `L2MatchingEngine` / `L3MatchingEngine`
- `BacktestConfig` / `BacktestResult`

### axon-rl

- `AxonEnv`(Gymnasium 兼容)
- `VecEnv`(向量化环境)
- `Step` / `Reset` / `Action` / `Observation`

### axon-llm

- `ReActAgent` / `Tool` trait
- `LLMBackend` / `OpenAICompatBackend` / `MockLLMBackend`
- `trading::PlaceOrderTool` / `QueryPortfolioTool` / `CancelOrderTool` / `ReplaceOrderTool`
- `trading::TradingBackend` trait
- `trading::RiskLimits` / `SafetyMode` / `RiskGate`
- `trading::TradingMetrics`
