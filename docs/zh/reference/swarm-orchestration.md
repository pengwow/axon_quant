# `axon-llm::swarm` 多 Agent 编排(0.6.0 P0 工作流 B 收口)

> 适用版本:`axon-llm` v0.6.0+
> 状态:**已实现**(工作流 B Batch 1-4 全收口)
> 上游 plan:`docs/superpowers/plans/2026-07-18-axon-quant-0.6.0.md` §3

`SwarmOrchestrator` 把 4 类 agent (Market / Risk / Execution / Audit) 串成可运行 pipeline,
配合 `HarnessBridge` 做最终裁决,`TradingBackend` 真下单到 `MockTradingBackend` / 交易所。
Python 端可直接构造 + 启动 + 注入信号,完整可观测。

## 1. 4-Agent 架构

```text
                 ┌────────────────────────────┐
                 │     SwarmOrchestrator       │
                 │  ┌──────────────────────┐   │
                 │  │   run_loop_arc       │   │◀──── inject_market_signal
                 │  │   dispatch()         │   │      inject_vote_response
                 │  └──────────────────────┘   │
                 │  ConsensusManager + Stats   │
                 └─────┬──────┬──────┬─────────┘
                       │      │      │      (mpsc inbox)
        ┌──────────────┘      │      └────────────┐
        │                     │                    │
   ┌────▼────┐         ┌─────▼─────┐         ┌────▼────┐
   │ Market  │  ───▶   │   Risk    │  ───▶   │Execution│  ──▶  PlaceOrderTool
   │ Agent   │ Market  │  Agent    │  Risk   │ Agent   │      QueryPortfolioTool
   │         │ Signal  │           │  Assess │         │      TradingBackend
   └─────────┘         └───────────┘         └─────────┘
        │                                          │
        │                                          ▼
        │                                    ┌──────────┐
        │                                    │  Audit   │
        │                                    │  Agent   │
        │                                    └──────────┘
        ▼
   MarketDataSource (Mock / WS / CSV)
```

**关键设计**:

- 每个 agent 持 `Box<dyn DeclarativeAgentRunner>`,**复用** DeclarativeAgent 抽象
- Agent 间通信用 `tokio::mpsc`,**不**用共享状态
- Orchestrator 主循环监听 inbox,按 `MessageContent` 路由
- `HarnessBridge` 由 SwarmOrchestrator 持有 + 各 agent 共享同一份 `Arc`

## 2. 消息路由表(`run_loop` 内)

| 收到的消息 | 处理动作 |
| --- | --- |
| `MarketAnalysis(signal)` | 创建 `TradeDecision` 投票,广播 `VoteRequest` 给 Risk + Execution |
| `RiskAssessment{approved=true}` | 转发给 Execution agent 生成 `ExecutionRequest` |
| `RiskAssessment{approved=false}` | 广播给 Audit 记录拒绝原因 |
| `ExecutionResult(result)` | 广播给 Audit 审计执行结果 |
| `VoteResult{passed=true}` | 调 `HarnessBridge.adjudicate()` 做最终裁决 |
| `VoteResponse(response)` | 投到 `ConsensusManager`,达法定人数回 `VoteResult` |
| `Shutdown` | 设置 `shutdown_requested=true`,主循环退出 |
| `Heartbeat` / 其他 | 忽略 |

### 投票共识与 Harness 集成

```text
VoteResponse → ConsensusManager
                    │
                    ▼ SimpleMajority (≥2 votes)
              VoteResult{passed=true}
                    │
                    ▼
        HarnessBridge.adjudicate(intent, ctx)
                    │
       ┌────────────┼────────────┬────────────┐
       ▼            ▼            ▼            ▼
   Approved     Rejected   CircuitBreak  NeedRevision
   (执行)      (审计)      (shutdown)     (回滚重分析)
```

`HarnessBridge` 缺省时降级为 `Adjudication::Approved`(零侵入模式,投票通过即批准)。

## 3. 关键模块

### 3.1 `DeclarativeAgentRunner` trait

```rust
#[async_trait]
pub trait DeclarativeAgentRunner: Send + Sync {
    fn id(&self) -> &AgentId;
    fn role(&self) -> AgentRole;
    fn status(&self) -> AgentStatus;
    async fn handle_message(&mut self, msg: AgentMessage) -> Result<RunnerOutput, SwarmError>;
}
```

- **Object safety**:trait 满足 object-safety,允许 `Arc<dyn DeclarativeAgentRunner>` 跨 task 持有
- **Sync 约束**:`Status` 返回 `Copy`,`handle_message` `&mut self` + 异步,允许并发

### 3.2 4 个 Agent

| Agent | 输入 | 输出 | 配置 |
| --- | --- | --- | --- |
| `MarketAgent` | `MarketDataSource` (tick) | `MarketSignal` | `symbols` + `price_change_threshold` |
| `RiskAgent` | `MarketSignal` | `RiskAssessment` | `RiskAgentConfig` (默认阈值) |
| `ExecutionAgent` | `RiskAssessment{approved=true}` | `ExecutionResult` (通过 `PlaceOrderTool`) | `TradingTools { place_order, query_portfolio }` |
| `AuditAgent` | `ExecutionResult` | (审计记录) | `AuditAgentConfig` (默认阈值) |

### 3.3 `PaperTradingBackend`

`PaperTradingBackend` 实现了 Stage K `TradingBackend` trait,模拟真实交易:

- **滑点**:`slippage_bps`(基点,买入上浮/卖出下浮)
- **手续费**:`commission_bps`(按 notional 收)
- **状态**:`cash` + `positions: HashMap<symbol, (qty, entry_price)>` + `last_prices`
- **价格更新**:`place_order` 后 `last_prices[symbol] = fill_price`
- **现金流**:Buy 扣 `notional * (1 + commission_bps)`,Sell 加 `notional * (1 - commission_bps)`

`get_balance()` 返回 `cash + Σ(qty * last_price)`(实时 NAV),`get_positions()` 返回 `(symbol, qty, entry_price, current_price, unrealized_pnl)`。

### 3.4 `SwarmOrchestrator::run_loop_arc`

`Arc<TokioMutex<SwarmOrchestrator>>` 跨 owner 共享,主循环:

```rust
pub async fn run_loop_arc(
    orchestrator: Arc<TokioMutex<Self>>,
    mut inbox_rx: mpsc::Receiver<AgentMessage>,
) {
    let tick = Duration::from_millis(guard.config.loop_tick_ms);
    loop {
        if guard.shutdown_requested { break; }
        let next = timeout(tick, inbox_rx.recv()).await;
        match next {
            Ok(Some(msg)) => dispatch(msg).await,
            Ok(None) => break,    // channel 关闭
            Err(_) => continue,   // timeout,检查 shutdown
        }
    }
}
```

**退出条件**:Shutdown 消息 / `request_shutdown()` / inbox channel 关闭(所有 agent outbox drop)。

## 4. Python 接入

### 4.1 典型用法

```python
from axon_quant.llm import (
    SwarmConfig, SwarmOrchestrator, MarketSignal, SignalType,
    TradingTools,
)
from axon_quant.trading import (
    MockTradingBackend, PlaceOrderTool, QueryPortfolioTool, RiskLimits,
)

# 1. 构造 orchestrator
config = SwarmConfig(vote_timeout_ms=5000, loop_tick_ms=100)
orch = SwarmOrchestrator(config)

# 2. 注册 4 类 agent(register_*_agent 自动启动 run_loop)
orch.register_market_agent(agent_id="m0", symbols=["BTC-USDT"])
orch.register_risk_agent(agent_id="r0")

# ExecutionAgent 必须传 tools(否则走 mock 模式)
backend = MockTradingBackend()
risk = RiskLimits(allowed_symbols=["BTC-USDT"])
place = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk)
query = QueryPortfolioTool(backend=backend)
tools = TradingTools(place_order=place, query_portfolio=query)
orch.register_execution_agent(agent_id="e0", tools=tools)

orch.register_audit_agent(agent_id="a0")

# 3. 注入 MarketSignal → orchestrator 触发投票 + 转发
orch.inject_market_signal(MarketSignal(
    symbol="BTC-USDT",
    signal_type=SignalType.Buy,
    confidence=0.9,
    reasoning="momentum breakout",
))

# 4. 读统计
import time; time.sleep(0.5)
stats = orch.stats()
print(stats["market_signals"], stats["votes_created"])

# 5. 关闭
orch.stop()
```

### 4.2 完整 Pipeline demo

参见 [`examples/18_harness/swarm_demo.py`](https://github.com/pengwow/axon_quant/blob/main/examples/18_harness/swarm_demo.py)。

## 5. 测试覆盖

| 测试文件 | 数量 | 内容 |
| --- | --- | --- |
| `python/tests/test_swarm_pipeline_e2e.py` | **25/25 ✅** | 枚举/数据结构 + 4 类 agent 注册 + lifecycle + inject + stats |
| `crates/axon-llm/src/swarm/` lib unittests | 全部通过 | DeclarativeAgentRunner / Orchestrator / Vote / 4 agent / market_data |
| `crates/axon-llm/src/trading/paper_backend.rs` lib unittests | 全部通过 | place_order / balance / position 滑点+手续费验证 |

**总测试数**:322 lib unittests + 74 integration + 3 doctests + 25 Python E2E = **424 全过**。

## 6. 现状与未实现项(基于 0.6.0)

### 已完成的 0.3.x / 0.4.x 路线图

- **`PlaceOrderTool` 三模式**: `DryRun` / `TwoPhase` / `Direct` 三种 `SafetyMode` 全部实现。
  - `DryRun` 为默认安全模式(有意设计,防止 LLM 直发订单)。
  - `TwoPhase` 内部用 `pending: Mutex<HashMap<token, PendingOrder>>` 跟踪待确认订单,4 个 e2e 测试覆盖:首调用返回 `confirm_token` / 二次带 token 真发 / 错误 token 拒绝 / token 单次消费。
  - `Direct` 直接调 backend 无拦截。
- **`RiskAgent` 基础限额**: 已实现 `max_order_notional` + `quantity > 0` 检查;合规时 `approved=true` + `risk_score=0.1` + 空 `violations`,违规时附带违规列表。
- **真实交易所接入**: `ExchangeTradingBackend`(`crates/axon-llm/src/trading/exchange.rs`)完整实现,把 `ExchangeAdapter`(Binance / OKX)适配为 `TradingBackend`;依赖 `trading-exchange` feature,`SymbolMap` 提供 LLM symbol ↔ 交易所 symbol 双向映射。注:同文件测试模块里有 8 处 `unimplemented!()`,是 `#[cfg(test)]` 内的 `MockAdapter` stub(测试路径不调用),非生产代码缺口。

### 未实现(0.6.0+ 路线图)

- **`RiskAgent` 高级风控**: `RiskAgentConfig` 已定义 `max_position` / `max_drawdown` 字段但**未做检查**;`risk_score` 目前只是二元(0.1 / 0.9);波动率 / VaR / 历史回撤窗口 / 仓位集中度等指标**未实现**。**0.6.0 收口部分能力**:`axon-risk` 0.6.0 新增跨 leg 风险约束(`check_leg_pair(portfolio, &LegPair) -> RiskResult` + `RiskReason::LegPairNetExposureExceeded` + `per_leg_var` + `stress_pair` / `stress_portfolio`),`RiskConfig.max_leg_pair_net_exposure` 默认 0.0(严格 delta 中性)。`RiskAgent` 接入这些 API 替换 happy path 是后续工作。
- **4-Agent pipeline 跨进程协调(分布式 swarm)**: 当前 `SwarmOrchestrator` 是单进程内 mpsc 通道;跨进程协调、共识状态机 `ConsensusManager` 持久化、`axon-distributed` 的 Ray Actor 化包装**未实现**。计划在 0.7.0+ 路线图。
- **单 Agent 跨进程复用**: `MarketAgent` / `RiskAgent` / `AuditAgent` 目前以独立 `tokio::task::spawn` 运行,跨进程调度 / 共享 LLM client / 全局 prompt cache 仍待设计。
