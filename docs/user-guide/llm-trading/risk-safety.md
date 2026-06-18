# LLM 交易风控与安全

> 适用版本:axon-llm v0.1.0+
> 前置阅读:[overview.md](overview.md) §4

本文档详述 axon-llm 的三道风控防线 + 失败模式 + 恢复策略。所有风控都是 **fail-closed**(任一阶段失败立刻拒绝,绝不进入后端)。

## 1. SafetyMode: DryRun / TwoPhase / Direct

### 1.1 语义

`SafetyMode` 是下单前最外层拦截:

| 模式 | 行为 | 典型用途 |
|---|---|---|
| `DryRun` | **不真下单**,仅 tracing 日志,返回 `OrderAck { status: "DryRun" }` | LLM 决策验证 / 集成测试 |
| `TwoPhase` | **两次确认**:第一次返回 `confirm_token`(uuid v4),第二次带相同 token 才真发 | 高风险操作的人工 in-the-loop 审批 |
| `Direct` | **直接调后端**,无任何拦截 | 生产实盘(已通过其他风控) |

### 1.2 TwoPhase 详细流程

```python
# 第一次调用
ack1 = place_order_tool.execute({
    "symbol": "BTC-USDT",
    "side": "Buy",
    "quantity": 0.1,
    "price": 50000.0,
    "confirm_token": None,
})
# ack1: { "status": "PendingConfirm", "confirm_token": "uuid-xxx", "order_id": None }

# 第二次调用(必须带相同 token)
ack2 = place_order_tool.execute({
    "symbol": "BTC-USDT",
    "side": "Buy",
    "quantity": 0.1,
    "price": 50000.0,
    "confirm_token": "uuid-xxx",
})
# ack2: { "status": "Filled", "order_id": "real-id", ... }
```

**注意**:`TwoPhase` 状态在进程内(`PendingOrder` 缓存),重启后 token 失效,需要重新走第一阶段。

### 1.3 选型建议

- **开发 / 联调 / CI 测试**:`DryRun`,绝不下单
- **LLM agent 灰度上线**:`TwoPhase`,人工确认关键操作
- **生产实盘(已有多重外部审计)**:`Direct`

## 2. RiskLimits: 静态规则

`RiskLimits` 是下单前的第二道防线,包含 4 个静态规则,任意一条失败立刻拒绝。

### 2.1 规则清单

| 规则 | 字段 | 检查逻辑 | 失败信息示例 |
|------|------|----------|--------------|
| 单笔最大名义 | `max_order_notional` | `quantity * price <= max_order_notional` | `"order notional 60000.0 exceeds limit 50000.0"` |
| 单日最大订单数 | `max_daily_orders` | `daily_count < max_daily_orders` | `"daily order count 101 exceeds limit 100"` |
| 单 symbol 最大持仓绝对值 | `max_position_abs` | `|current_qty + side_delta| <= max_position_abs` | `"projected position 0.6 exceeds max abs 0.5"` |
| 允许的 symbol 白名单 | `allowed_symbols` | `symbol ∈ allowed_symbols` | `"symbol 'DOGE-USDT' not in allowed list"` |

### 2.2 规则组合示例

```python
risk = RiskLimits(
    max_order_notional=50_000.0,   # 单笔 ≤ 5 万 USDT
    max_daily_orders=100,            # 每天 ≤ 100 单
    max_position_abs=10.0,           # 单 symbol ≤ 10 单位
    allowed_symbols={"BTC-USDT", "ETH-USDT"},  # 只能交易这两个
)
```

### 2.3 `max_position_abs` 详解

**计算公式**:`projected = current_position + side_delta`,其中:
- `current_position`:从 `backend.get_positions()` 查询的当前持仓
- `side_delta`:`Buy` → `+quantity`,`Sell` → `-quantity`
- 失败条件:`|projected| > max_position_abs`

**多场景示例**:

```text
初始:position = 0, max_abs = 0.5
├── Buy 0.3 -> projected = 0.3,  |0.3| = 0.3 ≤ 0.5 ✅ 通过
├── Buy 0.3 -> projected = 0.6,  |0.6| = 0.6 > 0.5 ❌ 拒绝
├── Sell 0.3 -> projected = 0,    |0.0| = 0.0 ≤ 0.5 ✅ 通过
└── Sell 0.8 -> projected = -0.8, |-0.8| = 0.8 > 0.5 ❌ 拒绝
```

**注意**:`max_position_abs` 是 **单 symbol 隔离** 的,每个 symbol 独立计算。允许 `BTC-USDT` 持仓 10 + `ETH-USDT` 持仓 10,互不影响。

## 3. RiskGate: 动态闸门

`RiskGate` 是第三道防线,处理"运行中状态"(连续失败次数、日内 PnL 突破阈值等)。

### 3.1 内置实现

| 类型 | 触发逻辑 | 依赖 |
|------|----------|------|
| `AlwaysOpenGate` | 永远放行(默认) | 无 |
| `RejectionCircuitBreaker` | 连续 N 次风控拒绝后开闸(冷却期后自动恢复) | 无(core lib 内置) |
| `RiskPnLCircuitBreaker` | 日 PnL 突破阈值后开闸 | `axon-risk`(feature = `trading-risk-extra`) |

### 3.2 `RejectionCircuitBreaker` 详解

```rust
let gate = RejectionCircuitBreaker::new(
    threshold: 5,        // 连续 5 次风控拒绝
    cooldown_ms: 60_000,  // 冷却 60 秒
);
```

**状态机**:

```text
        ┌──────────────────────────────────────┐
        ↓                                      │
    [Closed] ──连续 N 次拒绝──> [Open] ──cooldown 结束──> [HalfOpen] ──一次成功──> [Closed]
        ↑                                          │
        └────────────────失败(回到 Open)────────────┘
```

- **Closed**:正常放行
- **Open**:拒绝所有下单,返回 `CircuitBreakerOpen` 错误
- **HalfOpen**:放行一次试单,成功则关闸,失败则重新进入 Open

### 3.3 `RiskPnLCircuitBreaker` 详解

```rust
let gate = RiskPnLCircuitBreaker::new(
    daily_pnl_floor: -1000.0,  // 日 PnL 跌破 -1000 USDT 开闸
);
```

**与 `RejectionCircuitBreaker` 的区别**:

| 维度 | RejectionCircuitBreaker | RiskPnLCircuitBreaker |
|------|-------------------------|------------------------|
| 触发指标 | 连续风控拒绝次数 | 日 PnL 净值 |
| 适用场景 | LLM 决策出现异常重复拒绝 | 真实亏损触底保护 |
| 依赖 | 零(core lib) | `axon-risk`(feature gate) |
| 冷却 | 固定时间 | 跨日自动重置(UTC 0 点) |

## 4. 失败模式与恢复

### 4.1 失败分类

| 失败类型 | 失败位置 | 恢复策略 |
|---------|---------|---------|
| `RiskLimitsViolation` | RiskLimits::check | 修改 args,重新下单 |
| `CircuitBreakerOpen` | RiskGate | 等待 cooldown / 半开试单成功 |
| `BackendError::Network` | 后端 | 指数退避重试(应用方负责) |
| `BackendError::Rejected` | 后端 | 修正 args,重新下单 |
| `BackendError::InsufficientFunds` | 后端 | 减仓后再下单 |
| `BackendError::SymbolNotFound` | 后端 | 检查 symbol 拼写 |

### 4.2 错误响应统一格式

所有 tool 的失败都通过 `ToolError::ExecutionFailed(msg)` 返回,`msg` 包含机器可读前缀 + 人类可读描述:

```json
{
  "error_type": "ExecutionFailed",
  "source": "RiskLimits",
  "message": "RiskLimits: order notional 60000.0 exceeds limit 50000.0"
}
```

```json
{
  "error_type": "ExecutionFailed",
  "source": "RiskGate",
  "message": "RiskGate: circuit breaker open (rejections=5, cooldown_remaining_ms=42137)"
}
```

LLM agent 可以根据 `source` 字段决定重试 / 询问用户 / 改参数。

## 5. 安全最佳实践

### 5.1 启用顺序

1. **生产前必启用**:`RiskLimits`(基础规则)
2. **强烈推荐启用**:`RejectionCircuitBreaker`(防止 LLM 决策死循环)
3. **高敏场景启用**:`TwoPhase`(人工 in-the-loop 审批)
4. **可选启用**:`RiskPnLCircuitBreaker`(需 `trading-risk-extra` feature)

### 5.2 选型矩阵

| 场景 | SafetyMode | RiskLimits | RiskGate | TwoPhase |
|------|-----------|-----------|----------|----------|
| 单元测试 | DryRun | 关闭 | AlwaysOpen | 关 |
| 集成测试 | DryRun | 关闭 | AlwaysOpen | 关 |
| 回测评估 | Direct | 按需 | AlwaysOpen | 关 |
| 灰度 LLM agent | TwoPhase | 严 | RejectionCB | 开 |
| 生产实盘 | Direct | 严 | RejectionCB | 按需 |
| 高敏实盘 | TwoPhase | 严 | RiskPnLCB | 开 |

### 5.3 审计与日志

所有风控决策都会输出 tracing 日志:

```text
INFO axon_llm::trading::tools::place_order: RiskLimits check passed order_id=ord-xxx
WARN axon_llm::trading::tools::place_order: RiskLimits rejected reason="notional exceeds" order_id=ord-yyy
ERROR axon_llm::trading::tools::place_order: RiskGate blocked reason="circuit breaker open" order_id=ord-zzz
```

应用方应把这些日志接入自己的 ELK / Loki / Datadog 等日志后端,作为合规审计的输入。

## 下一步

- [指标与告警](metrics-alerting.md) —— 监控与告警策略
- [运维手册](operations-runbook.md) —— 故障排查
