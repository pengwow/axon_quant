# `axon-llm::swarm` 多 Agent 编排(0.3.0 P0 工作流 B 收口)

> 适用版本:`axon-llm` v0.2.0+
> 状态:**已实现**(工作流 B Batch 1-4 全收口)
> 上游 plan:`.axon-internal/plans/2026-07-06-v0.3.0-p0-implementation.md` §3

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
|---|---|
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
|---|---|---|---|
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
|---|---|---|
| `python/tests/test_swarm_pipeline_e2e.py` | **25/25 ✅** | 枚举/数据结构 + 4 类 agent 注册 + lifecycle + inject + stats |
| `crates/axon-llm/src/swarm/` lib unittests | 全部通过 | DeclarativeAgentRunner / Orchestrator / Vote / 4 agent / market_data |
| `crates/axon-llm/src/trading/paper_backend.rs` lib unittests | 全部通过 | place_order / balance / position 滑点+手续费验证 |

**总测试数**:322 lib unittests + 74 integration + 3 doctests + 25 Python E2E = **424 全过**。

## 6. 已知遗留(留作 0.3.x)

- `PlaceOrderTool` 默认 `dry_run` 模式,`direct` / `two_phase` 模式需要外部触发两步确认
- `RiskAgent` 默认 `approved=true` 走 happy path,真风控逻辑(波动率 / VaR / 限额)留作 0.3.x
- 4-Agent pipeline 跨进程协调(分布式 swarm)留作 0.4.0
- 真实交易所接入(`axon-llm::trading::exchange` 8 处 `unimplemented!()`)留作 0.3.x
