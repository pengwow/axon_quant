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
- `KlineBuffer`(0.16.0 新增,滚动 K 线缓冲)
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
- `decision::DecisionBackend` / `SdmCascadeDecisionLayer` / `CascadeConfig` / `RuleFallback`(0.15.0 新增,SDM 决策层)

### axon-kernel（0.22.0 新增）

- `Kernel` / `KernelResult` / `KernelConfig`(统一主回路:`run` / `run_history` / `replay_from` / `resume_replay`)
- `KernelCore` / `BarLifecycle`(九步 per-bar 编排) / `BarAggregator`
- `GridFrame` / `build_frames` / `BarGridBarrier`(多品种帧屏障,`pump.rs`)
- `StrategyHost` / `OrderIntent` / `OrderSpec` / `HostCtx`(策略宿主契约,`host.rs`)
- `ExecutionSink` / `NullSink` / `BacktestSink` / `PaperSink`(执行回调,`sink.rs`)
- `Journal` / `JournalEntry`(事件溯源 JSONL) / `RunManifest` / `ModelArtifact`(持久化,`persist.rs`)
- `RLStrategyHost` / `InferenceBackend`(`rl.rs`) / `KernelError`

### axon-worker（0.18.0 起编排层,0.22.0 扩展实时链路）

- `BacktestRunner` / `BacktestResult`(0.18.0,全 Rust 回测编排)
- `KernelPump` / `KernelPumpBatch` / `Gap`(`pump.rs`,行情 → 事件泵,缺口记录)
- `LiveSink` / `RiskCheck` / `VenueUpdate` / `GatewayAck` / `LiveOrderGateway`(`live_sink.rs`,0.22.0 实时执行侧)
- `ExchangeGateway`(`exchange_gateway.rs`,async `ExchangeAdapter` → sync `LiveOrderGateway` 桥接)
- `LiveDriver` / `PaperDriver`(`live_driver.rs`,实时 / 纸面统一驱动原语)
- `run_shadow` / `ShadowInput` / `ShadowReport` / `ShadowDiff`(`shadow.rs`,0.22.0 Shadow oracle:新旧编排路径 parity 比对)
- Python 绑定 `axon_quant.worker`:`BacktestRunner` / `Kernel` / `LiveDriver` / `PaperDriver` / `LiveRecovery` / `BacktestResult`(`Kernel` 与实时链路 0.22.0 起)
