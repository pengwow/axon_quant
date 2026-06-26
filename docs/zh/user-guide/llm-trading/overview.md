# LLM 交易架构

> 适用版本:axon-llm v0.2.0+
> 状态:🟢 与代码同步(Stage A~K 全部交付)

本文档描述 `axon-llm` 的交易链路架构。目标读者:正在把 LLM agent 接入生产交易系统的工程师。

## 1. 系统组件

axon-llm 交易链路由以下几部分组成:

1. **LLM Agent 层** —— `ReActAgent` / `OpenAICompatBackend` 等,负责接收用户 prompt、决定调用哪些 tool、把 tool 返回的 JSON 写入 LLM context。
2. **Tool 层** —— 4 个交易 tool:`PlaceOrderTool` / `QueryPortfolioTool` / `CancelOrderTool` / `ReplaceOrderTool`。每个 tool 实现 `Tool` trait(`async fn execute(&self, args: &str) -> Result<String, ToolError>`),接受 JSON 字符串参数,返回 JSON 字符串结果。
3. **风控层** —— 三道防线,在 tool 内部串联:
   - **SafetyMode**(`DryRun` / `TwoPhase` / `Direct`):控制是否真发订单
   - **RiskLimits**(`max_order_notional` / `max_daily_orders` / `max_position_abs` / `allowed_symbols`):静态风控规则
   - **RiskGate**(`AlwaysOpenGate` / `RejectionCircuitBreaker` / `RiskPnLCircuitBreaker`):动态风控闸门
4. **后端适配层** —— `TradingBackend` trait,4 个实现:
   - `MockTradingBackend`(默认,无 feature flag,用于测试)
   - `ExchangeTradingBackend`(feature = `trading-exchange`,对接 Binance/OKX)
   - `OmsTradingBackend`(feature = `trading-oms`,对接 axon-oms 状态机)
   - `BacktestTradingBackend`(feature = `trading-backtest`,对接 axon-backtest L1/L2/L3)
5. **监控层** —— `TradingMetrics`(自包含,无外部监控栈依赖):
   - 5 个 `LabeledCounter`(下单/撤单/改单/风控拒绝/后端失败)
   - 1 个 `LatencyHistogram`(端到端 execute 时延)
   - 1 个 gauge(单日订单数镜像自 `DailyCounter`)
   - 两种数据出口:`set_callback` 实时推送 / `snapshot` 主动拉取

## 2. 数据流

```text
[User Prompt]
   ↓
[ReActAgent 决策]
   ↓ JSON args
[Tool::execute(args: &str)]
   ↓ parse args
[RiskLimits::check]────── ❌ -> ToolError::ExecutionFailed
   ↓
[RiskGate::is_blocked]─── ❌ -> ToolError::ExecutionFailed
   ↓
[TradingBackend::place_order / cancel_order / replace_order]
   ↓
[TradingError / OrderAck] -> JSON
   ↓
[ReActAgent 把 JSON 写入 context,继续 LLM 推理]
```

关键点:
- 所有 tool 的 args / result 都是 **JSON 字符串**,LLM 工具透传最自然
- 风控 fail-closed(任一阶段失败立刻拒绝,不进入后端)
- 后端调用全部 `async`,tool 内部用 `tokio::Runtime::block_on` 桥接
- Python 端通过 PyO3 调用(详见 [Python 绑定](../../reference/python-bindings.md))

## 3. 后端选型决策树

```text
你要做什么?
├── 单元测试 / 集成测试 / 集成 CI
│   └── MockTradingBackend(零依赖,默认启用)
├── 对接真实交易所(Binance / OKX testnet / mainnet)
│   └── ExchangeTradingBackend(feature = trading-exchange)
├── 接入生产订单管理系统(axon-oms 状态机)
│   └── OmsTradingBackend(feature = trading-oms)
├── 在历史数据上模拟 LLM 决策(回测式评估)
│   └── BacktestTradingBackend(feature = trading-backtest)
└── 自定义场景(内部撮合器、纸交易等)
    └── 实现 TradingBackend trait
```

## 4. 安全模型(纵深防御)

axon-llm 的安全模型遵循 **纵深防御(defense-in-depth)** 原则,任何单一防线失败都还有下一道:

| 层级 | 名称 | 触发时机 | 失败行为 |
|------|------|---------|---------|
| L0 | **应用方 prompt 安全** | 调用方 | 完全由调用方负责(LLM 越狱防护、敏感数据脱敏) |
| L1 | **SafetyMode** | tool 入口 | `DryRun` 不下单 / `TwoPhase` 二次确认 / `Direct` 透传 |
| L2 | **RiskLimits** | 下单前 | 任一规则失败立刻拒绝(`fail-closed`) |
| L3 | **RiskGate** | 下单前 | 闸门开则放行,关则拒绝(动态熔断) |
| L4 | **TradingBackend** | 实际执行 | 由各后端实现做最末一道防护(交易所自带的风控、OMS 自带的风控) |

详细规则与失败模式见 [风控与安全](risk-safety.md)。

## 5. 监控模型(轻量 + 可插拔)

axon-llm **不内置** 任何外部监控栈(Prometheus exporter、Grafana dashboard、OpenTelemetry collector 等),只提供自包含的 `TradingMetrics` 收集器:

- 数据出口 1:**回调** —— `metrics.set_callback(|sample| { ... })`,实时推送到调用方注册的 sink
- 数据出口 2:**快照** —— `metrics.snapshot()` 主动拉取,返回 `Vec<MetricSample>`

应用方自行接到自己团队的监控后端:

- Rust 应用方:用 `axum_prometheus` / `metrics-exporter-prometheus` / 自定义 sink
- Python 应用方:用 `prometheus_client` / `opentelemetry-sdk` / 自定义推送

应用方集成示例见 [指标与告警](metrics-alerting.md) §3。

## 6. 核心 Crate 关系

```text
axon-llm(trading 子模块)
├── trading::tools  ──── PlaceOrderTool / QueryPortfolioTool / CancelOrderTool / ReplaceOrderTool
├── trading::risk   ──── SafetyMode / RiskLimits / RiskGate trait
├── trading::backend──── TradingBackend trait
├── trading::metrics──── LabeledCounter / LatencyHistogram / TradingMetrics
├── trading::circuit_breaker_gate
│   ├── RejectionCircuitBreaker(core lib,零依赖)
│   └── RiskPnLCircuitBreaker(feature = trading-risk-extra,包装 axon_risk::CircuitBreaker)
└── trading::python ───── PyO3 绑定(RiskLimits / MockTradingBackend / 4 tool / TradingMetrics)
```

## 下一步

- [风控与安全](risk-safety.md) —— 三道防线详解
- [指标与告警](metrics-alerting.md) —— 监控数据出口
- [运维手册](operations-runbook.md) —— 部署、升级、故障排查
