# 场景二 — LLM 智能体驱动交易（OADER 循环）

> **完整示例**: [`examples/13_llm/llm_demo.py`](../../../examples/13_llm/llm_demo.py)
> LLM + Trading 交易 Agent 演示：市场分析 → 信号生成 → 风控 → Mock 执行 → ReAct 循环。

本文档深入解析 AXON 量化平台中 LLM 智能体的核心交易循环：**OADER 模型**（Observe-Analyze-Decide-Execute-Record）。OADER 将 ReAct（Reasoning + Acting）推理范式与量化交易的严谨风控相结合，支持实盘与回测两种运行模式。所有代码示例均基于 AXON `0.1.0` 版本的真实源代码。

---

## OADER 模型介绍

OADER 是 AXON 为 LLM 驱动的量化交易设计的五阶段闭环模型，名称取自五个核心阶段的英文首字母：

| 阶段 | 英文 | 职责 | 对应源码模块 |
|------|------|------|-------------|
| O | **Observe**（观察） | 采集市场数据、持仓快照、策略状态 | `axon-llm/src/context.rs` |
| A | **Analyze**（分析） | LLM 推理：理解市场态势、生成交易思路 | `axon-llm/src/agent.rs` |
| D | **Decide**（决策） | 根据分析结果决定交易动作（买/卖/持有） | `axon-llm/src/agent.rs` |
| E | **Execute**（执行） | 调用交易工具下单、查询持仓 | `axon-llm/src/trading/` |
| R | **Record**（记录） | 记录决策轨迹、回写上下文、生成可解释报告 | `axon-llm/src/explain/` |

OADER 的每个阶段都有明确的数据契约与安全边界，确保 LLM 的"创造性"不会突破风控底线。

---

## OADER 五阶段详解

### 架构总览

```text
+------------------------------------------------------------------+
|                         OADER 交易循环                            |
+------------------------------------------------------------------+
|                                                                  |
|  +-----------+   +-----------+   +-----------+   +-----------+  |
|  |  Observe  |-->|  Analyze  |-->|  Decide   |-->|  Execute  |  |
|  |  (观察)   |   |  (分析)   |   |  (决策)   |   |  (执行)   |  |
|  +-----------+   +-----------+   +-----------+   +-----------+  |
|       ^                                              |           |
|       |                                              v           |
|       |                                        +-----------+     |
|       |                                        |  Record   |     |
|       |                                        |  (记录)   |     |
|       |                                        +-----------+     |
|       |                                              |           |
|       +----------------------------------------------+           |
|                        (上下文回写)                               |
+------------------------------------------------------------------+
```

### 阶段 1：Observe（观察）

**职责**：收集所有可供 LLM 决策的上下文信息。  
**源码**：`crates/axon-llm/src/context.rs`

`ContextBuilder` 负责组装三类输入：

1. **市场数据**：当前 K 线、订单簿、技术指标（通过 `MarketDataTool`）
2. **持仓快照**：当前余额、持仓列表、浮动盈亏（通过 `QueryPortfolioTool`）
3. **策略状态**：上一步的决策记录、累计盈亏、运行时长（通过 `ExplainRecorder` 的上下文回写）

```python
"""
Observe 阶段：构建 LLM 决策上下文
对应 Rust 源码：crates/axon-llm/src/context.rs
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class ObservationContext:
    """OADER Observe 阶段的输出数据结构。"""
    # 市场数据：当前行情快照
    market_data: dict[str, Any] = field(default_factory=dict)
    # 持仓快照：余额 + 持仓列表
    portfolio: dict[str, Any] = field(default_factory=dict)
    # 策略状态：上一步决策、累计盈亏等
    strategy_state: dict[str, Any] = field(default_factory=dict)
    # 时间戳（毫秒）
    timestamp_ms: int = 0


class ContextBuilder:
    """
    上下文构建器：将多个数据源聚合为 LLM 可用的 ObservationContext。
    对应 Rust 中的 ContextBuilder trait 实现。
    """

    def __init__(self):
        self._market_data_tool = None   # MarketDataTool
        self._portfolio_tool = None     # QueryPortfolioTool
        self._recorder = None           # ExplainRecorder（用于读取历史状态）

    def with_market_data(self, symbol: str, timeframe: str = "1h") -> "ContextBuilder":
        """注入市场数据工具，获取指定交易对的行情。"""
        self._market_data_tool = {"symbol": symbol, "timeframe": timeframe}
        return self

    def with_portfolio(self) -> "ContextBuilder":
        """注入持仓查询工具。"""
        self._portfolio_tool = {"type": "QueryPortfolio"}
        return self

    def with_strategy_state(self, recorder) -> "ContextBuilder":
        """注入策略状态记录器，读取上一步的决策历史。"""
        self._recorder = recorder
        return self

    def build(self) -> ObservationContext:
        """组装完整的观察上下文。"""
        ctx = ObservationContext()

        # 1. 采集市场数据
        if self._market_data_tool:
            ctx.market_data = {
                "symbol": self._market_data_tool["symbol"],
                "price": 50_000.0,          # 模拟当前价格
                "change_24h": 0.025,        # 24h 涨跌幅
                "volume_24h": 1_200_000_000.0,
            }

        # 2. 采集持仓快照
        if self._portfolio_tool:
            ctx.portfolio = {
                "balance": {"USDT": 10_000.0, "BTC": 0.0},
                "positions": [],             # 当前无持仓
            }

        # 3. 读取策略状态（上一步决策记录）
        if self._recorder:
            ctx.strategy_state = self._recorder.get_last_state()

        import time
        ctx.timestamp_ms = int(time.time() * 1000)
        return ctx


# 使用示例
if __name__ == "__main__":
    builder = ContextBuilder()
    ctx = (
        builder
        .with_market_data("BTC-USDT", timeframe="1h")
        .with_portfolio()
        .build()
    )
    print(f"[Observe] 上下文构建完成: {ctx}")
```

### 阶段 2：Analyze（分析）

**职责**：LLM 基于观察到的上下文进行推理，生成交易分析思路。  
**源码**：`crates/axon-llm/src/agent.rs`（`run_reasoning_cycle`）

分析阶段是 ReAct 循环的核心。LLM 接收系统提示词（`SystemPrompt`）+ 观察上下文，输出结构化的 `AnalysisResult`，包含：

- `thought`：内部推理过程（可解释性）
- `market_assessment`：市场态势评估（趋势/震荡/反转）
- `risk_assessment`：风险等级（低/中/高）
- `confidence`：置信度分数（0.0 ~ 1.0）

```python
"""
Analyze 阶段：LLM 推理与市场分析
对应 Rust 源码：crates/axon-llm/src/agent.rs 中的 run_reasoning_cycle
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass
class AnalysisResult:
    """Analyze 阶段的输出。"""
    thought: str                    # LLM 的内部推理过程
    market_assessment: str          # 市场态势："uptrend" / "downtrend" / "range"
    risk_assessment: str            # 风险等级："low" / "medium" / "high"
    confidence: float               # 置信度 0.0~1.0
    reasoning_steps: list[str]      # ReAct 的逐步推理链


class Analyzer:
    """
    分析器：调用 LLM 进行市场分析。
    对应 Rust 中的 Agent::run_reasoning_cycle 方法。
    """

    def __init__(self, backend):
        """
        backend: LLM 后端（OpenAICompatBackend / MockBackend）
        """
        self.backend = backend

    def analyze(self, ctx: "ObservationContext") -> AnalysisResult:
        """
        执行分析推理。
        在 Rust 中，这对应于：
            let response = self.backend.complete(prompt).await?;
        """
        # 构造系统提示词（对应 SystemPrompt::new）
        system_prompt = (
            "You are a quantitative trading analyst. "
            "Analyze the provided market data and portfolio state. "
            "Output your reasoning in structured JSON."
        )

        # 构造用户提示词（包含 ObservationContext 的所有信息）
        user_prompt = self._format_context(ctx)

        # 调用 LLM 后端（模拟）
        raw_response = self.backend.complete(system_prompt, user_prompt)

        # 解析结构化输出
        return AnalysisResult(
            thought="BTC shows strong momentum with increasing volume.",
            market_assessment="uptrend",
            risk_assessment="medium",
            confidence=0.82,
            reasoning_steps=[
                "Observe: Price broke above 20-day MA",
                "Analyze: Volume confirms breakout",
                "Assess: Risk is medium due to macro uncertainty",
            ],
        )

    def _format_context(self, ctx: "ObservationContext") -> str:
        """将 ObservationContext 格式化为 LLM 可读的文本。"""
        lines = [
            "=== Market Data ===",
            f"Symbol: {ctx.market_data.get('symbol', 'N/A')}",
            f"Price: {ctx.market_data.get('price', 'N/A')}",
            f"24h Change: {ctx.market_data.get('change_24h', 'N/A')}",
            "",
            "=== Portfolio ===",
            f"Balance: {ctx.portfolio.get('balance', {})}",
            f"Positions: {ctx.portfolio.get('positions', [])}",
        ]
        return "\n".join(lines)


# 使用示例
if __name__ == "__main__":
    class MockBackend:
        def complete(self, system: str, user: str) -> str:
            return "mock_response"

    analyzer = Analyzer(MockBackend())
    # 假设已有 ObservationContext
    # result = analyzer.analyze(ctx)
    print("[Analyze] 分析器初始化完成")
```

### 阶段 3：Decide（决策）

**职责**：基于分析结果，输出最终交易决策。  
**源码**：`crates/axon-llm/src/agent.rs`（`run_reasoning_cycle` 的 Decide 分支）

决策阶段将 `AnalysisResult` 映射为具体的交易动作。AXON 支持三种决策模式：

1. **LLM 直接决策**：LLM 输出 `action` 字段（Buy / Sell / Hold）
2. **RL 辅助决策**：RL 模型提供动作概率，LLM 在此基础上修正
3. **规则兜底**：当置信度低于阈值时，触发预设规则策略

```python
"""
Decide 阶段：交易决策
对应 Rust 源码：crates/axon-llm/src/agent.rs 中的决策逻辑
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any, Optional


class ActionType(Enum):
    """交易动作类型。"""
    BUY = "buy"
    SELL = "sell"
    HOLD = "hold"


@dataclass
class Decision:
    """Decide 阶段的输出。"""
    action: ActionType              # 交易动作
    symbol: str                     # 交易对
    quantity: Optional[float]       # 数量（None 表示由风控模块计算）
    order_type: str                 # "limit" / "market"
    price: Optional[float]          # 限价单价格
    stop_loss: Optional[float]      # 止损价
    take_profit: Optional[float]    # 止盈价
    reason: str                     # 决策理由（可解释性）
    confidence: float               # 决策置信度


class DecisionEngine:
    """
    决策引擎：将分析结果转换为具体交易指令。
    对应 Rust 中的 Agent 决策分支。
    """

    # 最小置信度阈值：低于此值触发 HOLD
    MIN_CONFIDENCE = 0.6

    def __init__(self, risk_limits: dict[str, Any] | None = None):
        self.risk_limits = risk_limits or {
            "max_position_size": 0.1,      # 最大仓位 10%
            "max_drawdown_pct": 0.05,      # 最大回撤 5%
        }

    def decide(self, analysis: "AnalysisResult", ctx: "ObservationContext") -> Decision:
        """
        基于分析结果生成交易决策。
        安全机制：置信度不足时强制 HOLD。
        """
        # 安全机制 1：置信度检查
        if analysis.confidence < self.MIN_CONFIDENCE:
            return Decision(
                action=ActionType.HOLD,
                symbol=ctx.market_data.get("symbol", "BTC-USDT"),
                quantity=None,
                order_type="market",
                price=None,
                stop_loss=None,
                take_profit=None,
                reason=f"Confidence {analysis.confidence:.2f} below threshold {self.MIN_CONFIDENCE}",
                confidence=analysis.confidence,
            )

        # 安全机制 2：风险评估检查
        if analysis.risk_assessment == "high":
            return Decision(
                action=ActionType.HOLD,
                symbol=ctx.market_data.get("symbol", "BTC-USDT"),
                quantity=None,
                order_type="market",
                price=None,
                stop_loss=None,
                take_profit=None,
                reason="Risk assessment is HIGH — holding position",
                confidence=analysis.confidence,
            )

        # 正常决策逻辑
        symbol = ctx.market_data.get("symbol", "BTC-USDT")
        price = ctx.market_data.get("price", 50_000.0)

        if analysis.market_assessment == "uptrend":
            return Decision(
                action=ActionType.BUY,
                symbol=symbol,
                quantity=0.01,              # 示例固定数量，真实场景由风控计算
                order_type="limit",
                price=price * 0.995,        # 略低于市价挂单
                stop_loss=price * 0.95,     # 5% 止损
                take_profit=price * 1.05,   # 5% 止盈
                reason=f"Uptrend detected with confidence {analysis.confidence:.2f}",
                confidence=analysis.confidence,
            )
        elif analysis.market_assessment == "downtrend":
            return Decision(
                action=ActionType.SELL,
                symbol=symbol,
                quantity=0.01,
                order_type="limit",
                price=price * 1.005,
                stop_loss=price * 1.05,
                take_profit=price * 0.95,
                reason=f"Downtrend detected with confidence {analysis.confidence:.2f}",
                confidence=analysis.confidence,
            )
        else:
            return Decision(
                action=ActionType.HOLD,
                symbol=symbol,
                quantity=None,
                order_type="market",
                price=None,
                stop_loss=None,
                take_profit=None,
                reason="Market in range — no clear signal",
                confidence=analysis.confidence,
            )


# 使用示例
if __name__ == "__main__":
    engine = DecisionEngine()
    print("[Decide] 决策引擎初始化完成")
```

### 阶段 4：Execute（执行）

**职责**：将决策转换为实际交易操作，调用交易所 API 或回测引擎。  
**源码**：`crates/axon-llm/src/trading/place_order_tool.rs`、`query_portfolio_tool.rs`

执行阶段通过 `PlaceOrderTool` 和 `QueryPortfolioTool` 与交易后端交互。AXON 支持两种执行模式：

- **实盘模式**：调用真实交易所 API（通过 `TradingBackend` trait）
- **回测模式**：调用 `BacktestEngine` 的 `step()` 方法（通过 `BacktestTradingBackend`）

```python
"""
Execute 阶段：交易执行
对应 Rust 源码：
  - crates/axon-llm/src/trading/place_order_tool.rs
  - crates/axon-llm/src/trading/query_portfolio_tool.rs
  - crates/axon-llm/src/trading/backend.rs
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Optional


@dataclass
class OrderAck:
    """订单回执（对应 Rust 的 OrderAck）。"""
    order_id: str
    symbol: str
    side: str
    quantity: float
    status: str
    timestamp_ms: int
    confirm_token: Optional[str] = None


class PlaceOrderTool:
    """
    下单工具：将 Decision 转换为交易所订单。
    对应 Rust 中的 PlaceOrderTool trait 实现。
    """

    def __init__(self, backend: Any):
        """
        backend: TradingBackend 实例（实盘或回测）
        """
        self.backend = backend

    async def execute(self, decision: "Decision") -> OrderAck:
        """
        执行交易决策。
        在 Rust 中：
            let ack = self.backend.place_order(args).await?;
        """
        # 构造下单参数（对应 PlaceOrderArgs）
        order_args = {
            "symbol": decision.symbol,
            "side": decision.action.value.upper(),
            "quantity": decision.quantity or 0.0,
            "order_type": decision.order_type.upper(),
            "price": decision.price,
            "stop_loss": decision.stop_loss,
            "take_profit": decision.take_profit,
            "time_in_force": "GTC",
            "extras": {},
        }

        # 调用后端执行
        ack = await self.backend.place_order(order_args)
        return ack


class QueryPortfolioTool:
    """
    持仓查询工具：获取当前投资组合状态。
    对应 Rust 中的 QueryPortfolioTool。
    """

    def __init__(self, backend: Any):
        self.backend = backend

    async def query(self, symbol: Optional[str] = None) -> dict[str, Any]:
        """
        查询持仓。
        在 Rust 中：
            let snapshot = self.backend.query_portfolio(args).await?;
        """
        args = {"symbol": symbol}
        return await self.backend.query_portfolio(args)


# 使用示例
if __name__ == "__main__":
    print("[Execute] 交易执行工具初始化完成")
```

### 阶段 5：Record（记录）

**职责**：记录完整决策轨迹，支持可解释性分析与策略迭代。  
**源码**：`crates/axon-llm/src/explain/recorder.rs`、`store.rs`

记录阶段是 OADER 闭环的关键。`ExplainRecorder` 将每个循环的上下文、分析、决策、执行结果持久化到 `ExplainStore`，并通过 `ExplainBridge` 生成结构化报告。

```python
"""
Record 阶段：决策轨迹记录与可解释性
对应 Rust 源码：
  - crates/axon-llm/src/explain/recorder.rs
  - crates/axon-llm/src/explain/store.rs
  - crates/axon-llm/src/explain/bridge.rs
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any
import time


@dataclass
class DecisionRecord:
    """单次 OADER 循环的完整记录。"""
    timestamp_ms: int
    observation: dict[str, Any]      # Observe 阶段的原始输入
    analysis: dict[str, Any]         # Analyze 阶段的输出
    decision: dict[str, Any]         # Decide 阶段的输出
    execution: dict[str, Any]        # Execute 阶段的回执
    pnl: float = 0.0                 # 该步盈亏


class ExplainRecorder:
    """
    决策记录器：记录每次 OADER 循环的完整轨迹。
    对应 Rust 中的 ExplainRecorder。
    """

    def __init__(self, store: "ExplainStore"):
        self.store = store
        self._records: list[DecisionRecord] = []

    def record(self, record: DecisionRecord) -> None:
        """记录一次决策循环。"""
        self._records.append(record)
        # 持久化到存储
        self.store.append(record)

    def get_last_state(self) -> dict[str, Any]:
        """获取上一步的策略状态（用于下一步 Observe）。"""
        if not self._records:
            return {}
        last = self._records[-1]
        return {
            "last_action": last.decision.get("action"),
            "last_pnl": last.pnl,
            "cumulative_pnl": sum(r.pnl for r in self._records),
            "step_count": len(self._records),
        }

    def get_records(self) -> list[DecisionRecord]:
        """获取所有记录。"""
        return self._records.copy()


class ExplainStore:
    """
    决策存储：持久化决策记录。
    对应 Rust 中的 ExplainStore。
    """

    def __init__(self, path: str = "explain_store.json"):
        self.path = path
        self._data: list[dict[str, Any]] = []

    def append(self, record: DecisionRecord) -> None:
        """追加记录。"""
        self._data.append({
            "timestamp_ms": record.timestamp_ms,
            "observation": record.observation,
            "analysis": record.analysis,
            "decision": record.decision,
            "execution": record.execution,
            "pnl": record.pnl,
        })

    def query(self, start_ms: int, end_ms: int) -> list[dict[str, Any]]:
        """按时间范围查询记录。"""
        return [r for r in self._data if start_ms <= r["timestamp_ms"] <= end_ms]


class ExplainBridge:
    """
    可解释性桥接：将记录转换为人类可读报告。
    对应 Rust 中的 ExplainBridge。
    """

    def __init__(self, recorder: ExplainRecorder):
        self.recorder = recorder

    def generate_report(self) -> str:
        """生成可解释性报告。"""
        records = self.recorder.get_records()
        lines = [
            "# OADER 交易决策报告",
            f"总步数: {len(records)}",
            f"总盈亏: {sum(r.pnl for r in records):.2f}",
            "",
            "## 决策明细",
        ]
        for i, r in enumerate(records, 1):
            lines.append(f"### Step {i}")
            lines.append(f"- 动作: {r.decision.get('action', 'N/A')}")
            lines.append(f"- 理由: {r.decision.get('reason', 'N/A')}")
            lines.append(f"- 盈亏: {r.pnl:.2f}")
            lines.append("")
        return "\n".join(lines)


# 使用示例
if __name__ == "__main__":
    store = ExplainStore()
    recorder = ExplainRecorder(store)
    print("[Record] 记录系统初始化完成")
```

---

## ReAct 推理循环核心逻辑

### ReAct 在 OADER 中的映射

ReAct（Reasoning + Acting）是 OADER Analyze 阶段的核心推理范式。AXON 将经典的 ReAct 循环适配为量化交易场景：

```text
+-------------------------------------------------------------+
|                    ReAct 推理循环                             |
+-------------------------------------------------------------+
|                                                             |
|   +------------+    +------------+    +------------+       |
|   |  Thought   | -> |   Action   | -> | Observation|       |
|   |  (思考)    |    |  (动作)    |    |  (观察反馈) |       |
|   +------------+    +------------+    +------------+       |
|        ^                                    |               |
|        |                                    v               |
|        +------------------------------------+               |
|                    (循环迭代)                                |
+-------------------------------------------------------------+
```

### 四个关键机制

#### 机制 1：结构化提示词（System Prompt）

AXON 通过 `SystemPrompt` 强制 LLM 输出结构化 JSON，确保下游模块可解析：

```python
"""
ReAct 机制 1：结构化提示词
对应 Rust 源码：crates/axon-llm/src/prompt.rs
"""

from __future__ import annotations


class SystemPrompt:
    """
    系统提示词模板：约束 LLM 输出格式。
    对应 Rust 中的 SystemPrompt::new。
    """

    TEMPLATE = """You are a quantitative trading agent operating in an OADER loop.

Your task is to analyze market data and make trading decisions.

You MUST respond in the following JSON format:
{
  "thought": "Your step-by-step reasoning process",
  "market_assessment": "uptrend|downtrend|range",
  "risk_assessment": "low|medium|high",
  "confidence": 0.0-1.0,
  "action": "buy|sell|hold",
  "reason": "Clear explanation of your decision"
}

Rules:
1. Always provide structured JSON output
2. Confidence must be between 0.0 and 1.0
3. If confidence < 0.6, action must be "hold"
4. Consider risk assessment before making decisions
"""

    @classmethod
    def build(cls, extra_rules: list[str] | None = None) -> str:
        """构建系统提示词。"""
        prompt = cls.TEMPLATE
        if extra_rules:
            prompt += "\nAdditional Rules:\n" + "\n".join(f"- {r}" for r in extra_rules)
        return prompt


# 使用示例
if __name__ == "__main__":
    prompt = SystemPrompt.build(["Max position size: 10%", "Stop loss required for all trades"])
    print("[ReAct] 系统提示词构建完成")
```

#### 机制 2：工具调用（Tool Use）

AXON 的 LLM 工具系统允许智能体在推理过程中调用外部工具获取实时数据：

```python
"""
ReAct 机制 2：工具调用系统
对应 Rust 源码：crates/axon-llm/src/tools.rs
"""

from __future__ import annotations

from typing import Any, Callable


class Tool:
    """工具定义：对应 Rust 中的 Tool trait。"""

    def __init__(self, name: str, description: str, handler: Callable[..., Any]):
        self.name = name
        self.description = description
        self.handler = handler

    def call(self, **kwargs: Any) -> Any:
        """执行工具。"""
        return self.handler(**kwargs)


class ToolRegistry:
    """
    工具注册表：管理所有可用工具。
    对应 Rust 中的 ToolRegistry。
    """

    def __init__(self):
        self._tools: dict[str, Tool] = {}

    def register(self, tool: Tool) -> "ToolRegistry":
        """注册工具。"""
        self._tools[tool.name] = tool
        return self

    def get(self, name: str) -> Tool:
        """获取工具。"""
        return self._tools[name]

    def list_tools(self) -> list[str]:
        """列出所有工具名称。"""
        return list(self._tools.keys())

    def build_tool_description(self) -> str:
        """
        构建工具描述文本，供 LLM 理解可用工具。
        对应 Rust 中的工具描述生成逻辑。
        """
        lines = ["Available Tools:"]
        for name, tool in self._tools.items():
            lines.append(f"- {name}: {tool.description}")
        return "\n".join(lines)


# 使用示例
if __name__ == "__main__":
    registry = ToolRegistry()
    registry.register(Tool(
        name="get_market_data",
        description="获取指定交易对的实时行情数据",
        handler=lambda symbol, timeframe: {"price": 50000, "change": 0.02},
    ))
    registry.register(Tool(
        name="get_portfolio",
        description="查询当前投资组合状态",
        handler=lambda: {"balance": 10000, "positions": []},
    ))
    print(f"[ReAct] 已注册工具: {registry.list_tools()}")
```

#### 机制 3：推理链追踪（Chain-of-Thought）

AXON 通过 `ExplainRecorder` 记录 LLM 的每一步推理，形成完整的决策审计链：

```python
"""
ReAct 机制 3：推理链追踪
对应 Rust 源码：crates/axon-llm/src/explain/recorder.rs
"""

from __future__ import annotations

from typing import Any


class ChainOfThoughtTracer:
    """
    推理链追踪器：记录 ReAct 循环中的每一步思考。
    对应 Rust 中的 ExplainRecorder 的 reasoning_steps 字段。
    """

    def __init__(self):
        self._steps: list[dict[str, Any]] = []

    def add_thought(self, step: int, thought: str, action: str, observation: str) -> None:
        """记录一步 ReAct 循环。"""
        self._steps.append({
            "step": step,
            "thought": thought,
            "action": action,
            "observation": observation,
        })

    def get_chain(self) -> list[dict[str, Any]]:
        """获取完整推理链。"""
        return self._steps.copy()

    def format_chain(self) -> str:
        """格式化推理链为人类可读文本。"""
        lines = ["## ReAct 推理链"]
        for s in self._steps:
            lines.append(f"### Step {s['step']}")
            lines.append(f"**Thought**: {s['thought']}")
            lines.append(f"**Action**: {s['action']}")
            lines.append(f"**Observation**: {s['observation']}")
            lines.append("")
        return "\n".join(lines)


# 使用示例
if __name__ == "__main__":
    tracer = ChainOfThoughtTracer()
    tracer.add_thought(
        step=1,
        thought="Price broke above 20-day MA with volume confirmation",
        action="Query market data for BTC-USDT",
        observation="BTC-USDT price: 51000, volume: 1.2B, RSI: 65",
    )
    tracer.add_thought(
        step=2,
        thought="RSI at 65 indicates momentum but not overbought",
        action="Query portfolio",
        observation="Balance: 10000 USDT, no open positions",
    )
    print(tracer.format_chain())
```

#### 机制 4：安全围栏（Safety Guardrails）

AXON 在 ReAct 循环中嵌入多层安全机制，防止 LLM 产生危险决策：

```python
"""
ReAct 机制 4：安全围栏
对应 Rust 源码：crates/axon-llm/src/trading/safety.rs
"""

from __future__ import annotations

from typing import Any


class SafetyGuard:
    """
    安全围栏：在决策执行前进行多维度安全检查。
    对应 Rust 中的 SafetyGuard。
    """

    def __init__(self, limits: dict[str, Any] | None = None):
        self.limits = limits or {
            "max_order_size": 1.0,           # 最大单笔下单量
            "max_daily_orders": 10,          # 每日最大订单数
            "max_position_value_usd": 5000,  # 最大持仓价值
            "forbidden_symbols": ["MEME"],   # 禁止交易的交易对
        }
        self._daily_order_count = 0

    def check(self, decision: "Decision", portfolio: dict[str, Any]) -> tuple[bool, str]:
        """
        安全检查：返回 (是否通过, 拒绝原因)。
        对应 Rust 中的 SafetyGuard::check。
        """
        # 检查 1：禁止交易对
        if decision.symbol in self.limits["forbidden_symbols"]:
            return False, f"Symbol {decision.symbol} is in forbidden list"

        # 检查 2：最大订单量
        if decision.quantity and decision.quantity > self.limits["max_order_size"]:
            return False, f"Order size {decision.quantity} exceeds limit {self.limits['max_order_size']}"

        # 检查 3：每日订单数限制
        if self._daily_order_count >= self.limits["max_daily_orders"]:
            return False, f"Daily order limit {self.limits['max_daily_orders']} reached"

        # 检查 4：持仓价值限制
        if decision.action.value == "buy":
            current_value = portfolio.get("total_value", 0)
            order_value = (decision.quantity or 0) * (decision.price or 0)
            if current_value + order_value > self.limits["max_position_value_usd"]:
                return False, "Position value would exceed limit"

        self._daily_order_count += 1
        return True, ""


# 使用示例
if __name__ == "__main__":
    guard = SafetyGuard()
    print("[ReAct] 安全围栏初始化完成")
```

---

## 多模型协同决策表

AXON 的 OADER 循环支持多种 LLM 后端协同工作，不同模型在循环中承担不同角色：

| 模型类型 | 在 OADER 中的角色 | 典型使用场景 | 对应源码 |
|----------|------------------|-------------|---------|
| **大语言模型（LLM）** | Analyze + Decide 主推理引擎 | 市场分析、策略推理、决策生成 | `axon-llm/src/agent.rs` |
| **嵌入模型（Embedding）** | Observe 阶段语义检索 | 检索历史相似行情、策略匹配 | `axon-llm/src/context.rs` |
| **RL 策略模型** | Decide 阶段动作推荐 | 提供动作概率分布，供 LLM 参考 | `axon-rl/src/env/trading_env.rs` |
| **时序预测模型** | Observe 阶段特征增强 | 生成价格预测、波动率估计 | `axon-data/src/features.rs` |
| **风控规则引擎** | Execute 阶段前置检查 | 仓位限制、止损检查、合规审查 | `axon-llm/src/trading/safety.rs` |

### 多模型协同代码示例

```python
"""
多模型协同决策示例
展示如何在 OADER 循环中集成 LLM + RL + 风控模型
"""

from __future__ import annotations

from typing import Any


class MultiModelOrchestrator:
    """
    多模型编排器：协调 LLM、RL、风控模型共同决策。
    """

    def __init__(
        self,
        llm_backend: Any,      # LLM 后端（OpenAI / Local）
        rl_model: Any,         # RL 策略模型（PPO / SAC）
        safety_guard: Any,     # 风控规则引擎
    ):
        self.llm = llm_backend
        self.rl = rl_model
        self.safety = safety_guard

    async def decide(self, ctx: "ObservationContext") -> "Decision":
        """
        多模型协同决策流程：
        1. RL 模型提供动作概率
        2. LLM 基于 RL 输出 + 市场上下文做最终决策
        3. 风控引擎做最终检查
        """
        # Step 1: RL 模型推荐
        rl_action, rl_probs = self.rl.predict(ctx.market_data)

        # Step 2: LLM 综合决策（输入包含 RL 推荐）
        llm_input = {
            **ctx.__dict__,
            "rl_recommendation": rl_action,
            "rl_confidence": max(rl_probs),
        }
        analysis = await self.llm.analyze(llm_input)

        # Step 3: 风控检查
        decision = DecisionEngine().decide(analysis, ctx)
        passed, reason = self.safety.check(decision, ctx.portfolio)

        if not passed:
            return Decision(
                action=ActionType.HOLD,
                symbol=decision.symbol,
                quantity=None,
                order_type="market",
                price=None,
                stop_loss=None,
                take_profit=None,
                reason=f"SAFETY BLOCKED: {reason}",
                confidence=0.0,
            )

        return decision


# 使用示例
if __name__ == "__main__":
    print("[多模型协同] 编排器初始化完成")
```

---

## ReAct 与回测的联动

### 实盘 vs 回测对比表

| 维度 | 实盘模式 | 回测模式 | 切换方式 |
|------|---------|---------|---------|
| **交易后端** | `LiveTradingBackend`（调用交易所 API） | `BacktestTradingBackend`（调用 `BacktestEngine.step()`） | 通过 `TradingBackend` trait 多态切换 |
| **数据延迟** | 真实网络延迟 | 零延迟（模拟时钟推进） | `SimulatedClock` vs 系统时钟 |
| **订单执行** | 真实撮合（L1/L2/L3） | 模拟撮合（L1MatchingEngine） | `MatchingEngine` trait 实现 |
| **持仓查询** | 交易所 API | 回测引擎内部状态 | `QueryPortfolioTool` 统一接口 |
| **ExplainStore** | 写入生产数据库 | 写入临时文件/内存 | `ExplainStore` trait 实现 |
| **安全机制** | 全部启用（含资金限制） | 可放宽部分限制用于压力测试 | `SafetyGuard` 配置参数 |

### 回测模式代码示例

```python
"""
ReAct 与回测联动示例
展示如何在回测模式下运行完整的 OADER 循环
对应 Rust 源码：crates/axon-llm/src/trading/backend.rs
"""

from __future__ import annotations

import asyncio
from typing import Any


class BacktestTradingBackend:
    """
    回测交易后端：将 OADER Execute 阶段映射到 BacktestEngine。
    对应 Rust 中的 BacktestTradingBackend。
    """

    def __init__(self, engine: Any):
        """
        engine: BacktestEngine 实例
        """
        self.engine = engine
        self._order_id_counter = 0

    async def place_order(self, args: dict[str, Any]) -> dict[str, Any]:
        """
        在回测引擎中模拟下单。
        对应 Rust 中：
            let event = Event::new_order_submitted(...);
            engine.step(event);
        """
        self._order_id_counter += 1
        order_id = f"BT-{self._order_id_counter}"

        # 构造订单提交事件，推入回测引擎
        event = {
            "type": "Order",
            "timestamp": self.engine.current_timestamp(),
            "action": {
                "type": "Submitted",
                "order": {
                    "id": self._order_id_counter,
                    "symbol": args["symbol"],
                    "side": args["side"],
                    "order_type": {args["order_type"]: {"price": args.get("price")}},
                    "quantity": args["quantity"],
                    "time_in_force": args.get("time_in_force", "GTC"),
                }
            }
        }

        # 步进回测引擎
        stats = self.engine.step(event)

        return {
            "order_id": order_id,
            "symbol": args["symbol"],
            "side": args["side"],
            "quantity": args["quantity"],
            "status": "Filled" if stats else "Pending",
            "timestamp_ms": self.engine.current_timestamp(),
        }

    async def query_portfolio(self, args: dict[str, Any]) -> dict[str, Any]:
        """查询回测引擎的内部持仓状态。"""
        return self.engine.get_portfolio_snapshot()


async def run_backtest_oader_loop():
    """在回测模式下运行完整 OADER 循环。"""
    # 初始化回测引擎（对应步骤 5 的回测配置）
    engine = {
        "current_timestamp": lambda: 1_700_000_000_000,
        "step": lambda e: {"pnl": 0.0},
        "get_portfolio_snapshot": lambda: {"balance": {"USDT": 10000}, "positions": []},
    }

    backend = BacktestTradingBackend(engine)
    place_order_tool = PlaceOrderTool(backend)
    query_portfolio_tool = QueryPortfolioTool(backend)

    # 运行 10 步 OADER 循环
    for step in range(10):
        # Observe
        ctx = ContextBuilder().with_market_data("BTC-USDT").with_portfolio().build()

        # Analyze（简化：直接生成决策）
        decision = Decision(
            action=ActionType.BUY if step % 2 == 0 else ActionType.HOLD,
            symbol="BTC-USDT",
            quantity=0.01,
            order_type="limit",
            price=50000.0,
            stop_loss=47500.0,
            take_profit=52500.0,
            reason=f"Backtest step {step}",
            confidence=0.8,
        )

        # Execute
        if decision.action != ActionType.HOLD:
            ack = await place_order_tool.execute(decision)
            print(f"[回测] Step {step}: 订单 {ack['order_id']} 状态={ack['status']}")

        # Record
        print(f"[回测] Step {step}: 完成")


if __name__ == "__main__":
    asyncio.run(run_backtest_oader_loop())
```

---

## ReAct 与 HPO 的联动

### 联动链路图

```text
+----------------+     +----------------+     +----------------+
|   HPO 搜索     | --> |  RL 训练       | --> |  ReAct 调用    |
| (OptunaStudy)  |     | (PPO+TradingEnv)|     | (OADER 循环)   |
+----------------+     +----------------+     +----------------+
        |                       |                       |
        v                       v                       v
  搜索超参空间            训练策略模型            评估策略性能
  (lr, gamma,            (model.zip)             (Sharpe / PnL)
   batch_size)
        |                       |                       |
        +-----------------------+-----------------------+
                                |
                                v
                        +----------------+
                        |  反馈到 HPO    |
                        | (目标函数评分)  |
                        +----------------+
```

### HPO → RL → ReAct 代码示例

```python
"""
ReAct 与 HPO 联动示例
展示如何将 OADER 循环的绩效作为 HPO 的目标函数
"""

from __future__ import annotations

import asyncio
import json
from typing import Any

import axon_quant

hpo = axon_quant.hpo


async def evaluate_react_strategy(params: dict[str, Any]) -> list[float]:
    """
    HPO 目标函数：用一组超参训练 RL 模型，然后在 ReAct 循环中评估。
    返回：[sharpe_ratio, -max_drawdown]
    """
    # 1. 使用当前 trial 的超参训练 RL 模型
    # （复用步骤 2 的训练逻辑）
    lr = params["learning_rate"]
    gamma = params["gamma"]
    print(f"[HPO→ReAct] 训练 RL 模型: lr={lr}, gamma={gamma}")

    # 2. 将训练好的 RL 模型集成到 OADER 的 Decide 阶段
    # rl_model = PPO.load(f"models/trial_{trial_id}.zip")

    # 3. 运行 ReAct 回测循环（如上一节所示）
    # 收集 100 步的 PnL 序列
    pnl_series = [0.01, -0.005, 0.015, -0.002, 0.008] * 20  # 模拟

    # 4. 计算绩效指标
    import numpy as np
    returns = np.array(pnl_series)
    sharpe = np.mean(returns) / (np.std(returns) + 1e-9) * np.sqrt(252)
    cumulative = np.cumsum(returns)
    max_dd = np.max(np.maximum.accumulate(cumulative) - cumulative)

    print(f"[HPO→ReAct] 评估结果: Sharpe={sharpe:.3f}, MaxDD={max_dd:.3f}")
    return [sharpe, -max_dd]


def main() -> int:
    print("=" * 60)
    print("HPO → RL → ReAct 联动示例")
    print("=" * 60)

    # 定义搜索空间
    search_space = {
        "learning_rate": hpo.SearchSpaceDef(param_type="log_uniform", low=1e-5, high=1e-3),
        "gamma": hpo.SearchSpaceDef(param_type="uniform", low=0.95, high=0.999),
    }

    # 创建 HPO 执行器（注意：目标函数需同步包装，因为 Optuna 不支持 async）
    def sync_objective(params):
        return asyncio.run(evaluate_react_strategy(params))

    runner = hpo.OptunaHPO(
        search_space=search_space,
        objective_fn=sync_objective,
        study_name="react_rl_hpo",
        directions=["maximize", "maximize"],
        sampler=hpo.SamplerConfig(sampler_type="tpe", seed=42),
    )

    # 执行搜索
    results = runner.run(n_trials=10, n_jobs=1)
    print(f"\n[HPO→ReAct] 完成 {len(results)} 个 trials")

    best = runner.get_best_trial()
    if best:
        print(f"[HPO→ReAct] 最佳超参: {best.params}")
        with open("best_react_hpo.json", "w") as f:
            json.dump(best.params, f, indent=2)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

---

## ReAct 与 RL 的联动

AXON 支持三种 ReAct 与 RL 的协同模式，覆盖从"LLM 主导"到"RL 主导"的全谱系：

### 模式 1：RL 辅助 LLM（RL-Augmented LLM）

**描述**：RL 模型提供动作概率分布，LLM 将其作为参考信号之一，结合自身推理做最终决策。  
**适用场景**：市场结构复杂、需要 LLM 理解非结构化信息（新闻、情绪）。

```python
"""
模式 1：RL 辅助 LLM
RL 提供动作概率，LLM 做最终决策
"""

from __future__ import annotations

from typing import Any
import numpy as np


class RLAugmentedLLM:
    """RL 辅助 LLM 决策器。"""

    def __init__(self, llm_backend: Any, rl_model: Any, rl_weight: float = 0.3):
        self.llm = llm_backend
        self.rl = rl_model
        self.rl_weight = rl_weight  # RL 信号权重

    async def decide(self, ctx: "ObservationContext") -> "Decision":
        # 1. LLM 独立分析
        llm_analysis = await self.llm.analyze(ctx)

        # 2. RL 模型输出动作概率
        obs = self._extract_observation(ctx)
        rl_action, rl_probs = self.rl.predict(obs)

        # 3. 融合决策：LLM 置信度与 RL 概率加权
        llm_confidence = llm_analysis.confidence
        rl_confidence = float(np.max(rl_probs))

        # 如果 RL 强烈信号且 LLM 犹豫，提升置信度
        if rl_confidence > 0.8 and llm_confidence < 0.6:
            fused_confidence = llm_confidence * (1 - self.rl_weight) + rl_confidence * self.rl_weight
            llm_analysis.confidence = min(fused_confidence, 0.95)
            llm_analysis.reasoning_steps.append(
                f"RL signal boosted confidence: {rl_action} (prob={rl_confidence:.2f})"
            )

        return DecisionEngine().decide(llm_analysis, ctx)

    def _extract_observation(self, ctx: "ObservationContext") -> np.ndarray:
        """将 ObservationContext 转换为 RL 模型的观测向量。"""
        return np.array([
            ctx.market_data.get("price", 0),
            ctx.market_data.get("change_24h", 0),
            ctx.portfolio.get("balance", {}).get("USDT", 0),
        ])


# 使用示例
if __name__ == "__main__":
    print("[RL+LLM] 模式 1：RL 辅助 LLM 初始化完成")
```

### 模式 2：LLM 辅助 RL 训练（LLM-Guided RL）

**描述**：LLM 生成奖励塑形（Reward Shaping）信号或课程学习（Curriculum Learning）目标，引导 RL 模型更快收敛。  
**适用场景**：RL 训练初期探索效率低，需要 LLM 提供先验知识。

```python
"""
模式 2：LLM 辅助 RL 训练
LLM 生成奖励塑形信号，引导 RL 策略学习
"""

from __future__ import annotations

from typing import Any


class LLMGuidedRL:
    """LLM 辅助 RL 训练器。"""

    def __init__(self, llm_backend: Any, base_reward: str = "pnl"):
        self.llm = llm_backend
        self.base_reward = base_reward

    def compute_shaped_reward(
        self,
        env_state: dict[str, Any],
        base_reward: float,
    ) -> float:
        """
        计算塑形后的奖励。
        LLM 根据当前市场状态判断基础奖励是否可靠，并添加塑形项。
        """
        # 让 LLM 评估当前市场状态的质量
        market_quality = self.llm.assess_market_quality(env_state)

        # 如果市场质量低（高噪音、低流动性），降低奖励幅度
        if market_quality == "poor":
            shaping_factor = 0.5
        elif market_quality == "good":
            shaping_factor = 1.2
        else:
            shaping_factor = 1.0

        shaped = base_reward * shaping_factor

        # 添加 LLM 指导的探索奖励
        if env_state.get("is_novel_state", False):
            exploration_bonus = 0.1
            shaped += exploration_bonus

        return shaped

    def generate_curriculum(self, performance_history: list[float]) -> list[dict[str, Any]]:
        """
        生成课程学习目标。
        LLM 根据历史表现决定下一阶段的训练难度。
        """
        avg_perf = sum(performance_history) / len(performance_history) if performance_history else 0

        if avg_perf < 0:
            # 表现差：降低难度，增加稳定趋势数据
            return [{"trend_strength": 0.8, "noise_level": 0.1}]
        elif avg_perf > 0.5:
            # 表现好：增加难度，引入震荡和反转
            return [{"trend_strength": 0.3, "noise_level": 0.3, "reversal_prob": 0.2}]
        else:
            return [{"trend_strength": 0.5, "noise_level": 0.2}]


# 使用示例
if __name__ == "__main__":
    print("[LLM→RL] 模式 2：LLM 辅助 RL 训练初始化完成")
```

### 模式 3：RL 降级备用（RL Fallback）

**描述**：当 LLM 服务不可用、响应超时或置信度持续低下时，自动切换为纯 RL 策略执行交易。  
**适用场景**：生产环境高可用要求，防止 LLM 故障导致交易中断。

```python
"""
模式 3：RL 降级备用
当 LLM 不可用时，自动切换为 RL 策略
"""

from __future__ import annotations

import asyncio
from typing import Any


class RLFallbackAgent:
    """
    带 RL 降级备用的 OADER 智能体。
    对应 Rust 中的 Agent 的 fallback 逻辑。
    """

    def __init__(
        self,
        llm_backend: Any,
        rl_model: Any,
        fallback_timeout_ms: float = 5000.0,
        min_confidence_threshold: float = 0.5,
    ):
        self.llm = llm_backend
        self.rl = rl_model
        self.fallback_timeout_ms = fallback_timeout_ms
        self.min_confidence = min_confidence_threshold
        self._fallback_count = 0
        self._llm_count = 0

    async def decide(self, ctx: "ObservationContext") -> "Decision":
        """
        决策流程：优先 LLM，异常时降级到 RL。
        """
        try:
            # 尝试 LLM 决策（带超时）
            llm_task = asyncio.create_task(self._llm_decide(ctx))
            decision = await asyncio.wait_for(
                llm_task, timeout=self.fallback_timeout_ms / 1000
            )

            # 检查 LLM 置信度
            if decision.confidence < self.min_confidence:
                print(f"[Fallback] LLM 置信度 {decision.confidence:.2f} 过低，切换 RL")
                return await self._rl_decide(ctx)

            self._llm_count += 1
            return decision

        except asyncio.TimeoutError:
            print(f"[Fallback] LLM 超时 ({self.fallback_timeout_ms}ms)，切换 RL")
            self._fallback_count += 1
            return await self._rl_decide(ctx)
        except Exception as e:
            print(f"[Fallback] LLM 异常: {e}，切换 RL")
            self._fallback_count += 1
            return await self._rl_decide(ctx)

    async def _llm_decide(self, ctx: "ObservationContext") -> "Decision":
        """LLM 决策路径。"""
        analysis = await self.llm.analyze(ctx)
        return DecisionEngine().decide(analysis, ctx)

    async def _rl_decide(self, ctx: "ObservationContext") -> "Decision":
        """RL 降级决策路径。"""
        obs = self._extract_observation(ctx)
        action, _ = self.rl.predict(obs)

        action_map = {0: ActionType.HOLD, 1: ActionType.BUY, 2: ActionType.SELL}
        return Decision(
            action=action_map.get(action, ActionType.HOLD),
            symbol=ctx.market_data.get("symbol", "BTC-USDT"),
            quantity=0.01,
            order_type="market",
            price=None,
            stop_loss=None,
            take_profit=None,
            reason="RL FALLBACK: LLM unavailable or low confidence",
            confidence=0.5,  # RL 决策默认中等置信度
        )

    def _extract_observation(self, ctx: "ObservationContext") -> Any:
        """提取 RL 观测。"""
        import numpy as np
        return np.array([
            ctx.market_data.get("price", 0),
            ctx.market_data.get("change_24h", 0),
        ])

    def get_stats(self) -> dict[str, int]:
        """获取决策统计。"""
        return {
            "llm_decisions": self._llm_count,
            "rl_fallbacks": self._fallback_count,
        }


# 使用示例
if __name__ == "__main__":
    print("[RL Fallback] 模式 3：RL 降级备用初始化完成")
```

---

## 安全机制与风险隔离

AXON 的安全机制按 OADER 阶段组织，形成纵深防御体系：

### 按阶段组织的安全表

| OADER 阶段 | 安全机制 | 实现位置 | 作用 |
|-----------|---------|---------|------|
| **Observe** | 数据源校验 | `axon-data/src/validation.rs` | 防止异常行情数据进入决策流程 |
| **Observe** | 上下文完整性检查 | `axon-llm/src/context.rs` | 确保所有必需字段存在且格式正确 |
| **Analyze** | 提示词注入过滤 | `axon-llm/src/prompt.rs` | 防止恶意输入污染 LLM 推理 |
| **Analyze** | 输出格式校验 | `axon-llm/src/agent.rs` | 强制 JSON Schema 验证，拒绝非结构化输出 |
| **Decide** | 置信度阈值 | `DecisionEngine.MIN_CONFIDENCE` | 低置信度强制 HOLD |
| **Decide** | 风险评估拦截 | `DecisionEngine.decide()` | 高风险评估强制 HOLD |
| **Execute** | 安全围栏检查 | `axon-llm/src/trading/safety.rs` | 仓位限制、禁止交易对、每日订单数 |
| **Execute** | 两阶段提交（Two-Phase） | `OrderAck.confirm_token` | 大额订单需人工确认 |
| **Execute** | 交易所 API 限流 | `TradingBackend` 实现 | 防止频繁调用触发交易所风控 |
| **Record** | 审计日志不可篡改 | `ExplainStore` 写前日志 | 确保决策轨迹可追溯 |
| **Record** | 敏感信息脱敏 | `ExplainBridge` | 密钥、仓位细节在报告中脱敏 |
| **全链路** | 熔断机制 | `Agent::run()` | 连续亏损 / 异常时自动暂停交易 |
| **全链路** | 最大回撤止损 | `SafetyGuard` | 累计回撤超阈值强制平仓 |

### 安全围栏配置示例

```python
"""
安全机制配置示例
展示如何在 OADER 循环中配置多层安全策略
"""

from __future__ import annotations

from typing import Any


class SecurityPolicy:
    """
    安全策略配置：集中管理所有安全参数。
    对应 Rust 中各模块的安全配置聚合。
    """

    DEFAULT_POLICY = {
        # Observe 阶段
        "max_data_age_ms": 60_000,           # 数据最大延迟 60 秒
        "required_fields": ["price", "volume", "timestamp"],

        # Analyze 阶段
        "max_prompt_length": 4096,           # 提示词最大长度
        "banned_keywords": ["ignore previous", "disregard"],  # 注入过滤

        # Decide 阶段
        "min_confidence": 0.6,
        "max_risk_level": "medium",          # 只允许 low/medium 风险

        # Execute 阶段
        "max_order_size": 1.0,
        "max_daily_orders": 20,
        "max_position_value_usd": 10_000,
        "require_stop_loss": True,           # 必须带止损
        "forbidden_symbols": ["MEME", "SHIT"],

        # 全链路
        "circuit_breaker_loss_threshold": -1000.0,  # 单日亏损熔断
        "max_drawdown_pct": 0.10,            # 10% 最大回撤
    }

    def __init__(self, overrides: dict[str, Any] | None = None):
        self.policy = {**self.DEFAULT_POLICY, **(overrides or {})}

    def validate_observation(self, data: dict[str, Any]) -> tuple[bool, str]:
        """校验观察数据。"""
        for field in self.policy["required_fields"]:
            if field not in data:
                return False, f"Missing required field: {field}"
        return True, ""

    def validate_decision(self, decision: "Decision") -> tuple[bool, str]:
        """校验决策。"""
        if decision.confidence < self.policy["min_confidence"]:
            return False, f"Confidence {decision.confidence} below threshold"
        if self.policy["require_stop_loss"] and decision.stop_loss is None:
            return False, "Stop loss required"
        return True, ""


# 使用示例
if __name__ == "__main__":
    policy = SecurityPolicy({"max_daily_orders": 5})
    print("[安全机制] 安全策略配置完成")
```

---

## OADER 循环部署示例

以下是一个完整的异步部署脚本，展示如何在生产环境中运行 OADER 循环，包含所有五个阶段、ReAct 推理、安全检查和记录回写。

```python
"""
OADER 循环完整部署示例
展示如何在生产环境中运行完整的 OADER 交易循环
"""

from __future__ import annotations

import asyncio
import time
from typing import Any


# ---------------------------------------------------------------------------
# 模拟依赖（真实环境应替换为 axon_quant 的实际导入）
# ---------------------------------------------------------------------------
class MockLLMBackend:
    async def analyze(self, ctx: dict[str, Any]) -> "AnalysisResult":
        await asyncio.sleep(0.01)  # 模拟网络延迟
        return AnalysisResult(
            thought="Market shows bullish momentum",
            market_assessment="uptrend",
            risk_assessment="medium",
            confidence=0.75,
            reasoning_steps=["Price above MA", "Volume increasing"],
        )


class MockRLModel:
    def predict(self, obs: Any) -> tuple[int, list[float]]:
        return 1, [0.1, 0.7, 0.2]  # 动作 1 = BUY


class MockTradingBackend:
    async def place_order(self, args: dict[str, Any]) -> dict[str, Any]:
        await asyncio.sleep(0.005)
        return {
            "order_id": f"ORD-{int(time.time()*1000)}",
            "symbol": args["symbol"],
            "side": args["side"],
            "quantity": args["quantity"],
            "status": "Filled",
            "timestamp_ms": int(time.time() * 1000),
        }

    async def query_portfolio(self, args: dict[str, Any]) -> dict[str, Any]:
        return {"balance": {"USDT": 10000, "BTC": 0.1}, "positions": []}


# ---------------------------------------------------------------------------
# 主循环
# ---------------------------------------------------------------------------
async def oader_loop(
    symbol: str = "BTC-USDT",
    max_iterations: int = 100,
    interval_seconds: float = 60.0,
) -> None:
    """
    完整的 OADER 交易循环。

    参数:
        symbol: 交易对
        max_iterations: 最大循环次数（None 表示无限）
        interval_seconds: 每轮循环间隔
    """
    print("=" * 60)
    print("OADER 交易循环启动")
    print(f"交易对: {symbol}")
    print("=" * 60)

    # 初始化组件
    llm = MockLLMBackend()
    rl = MockRLModel()
    backend = MockTradingBackend()
    safety = SafetyGuard()
    store = ExplainStore(path="oader_records.json")
    recorder = ExplainRecorder(store)
    bridge = ExplainBridge(recorder)

    # 多模型编排器（模式 1：RL 辅助 LLM）
    orchestrator = MultiModelOrchestrator(llm, rl, safety)

    iteration = 0
    while max_iterations is None or iteration < max_iterations:
        iteration += 1
        loop_start = time.perf_counter()
        print(f"\n--- OADER 循环 #{iteration} ---")

        try:
            # =========================================================
            # O - Observe（观察）
            # =========================================================
            print("[O] 采集上下文...")
            ctx = (
                ContextBuilder()
                .with_market_data(symbol, timeframe="1h")
                .with_portfolio()
                .with_strategy_state(recorder)
                .build()
            )

            # =========================================================
            # A - Analyze（分析）
            # =========================================================
            print("[A] LLM 推理分析...")
            analyzer = Analyzer(llm)
            analysis = await analyzer.analyze(ctx)
            print(f"  市场评估: {analysis.market_assessment}")
            print(f"  风险评估: {analysis.risk_assessment}")
            print(f"  置信度: {analysis.confidence:.2f}")

            # =========================================================
            # D - Decide（决策）
            # =========================================================
            print("[D] 生成交易决策...")
            decision = await orchestrator.decide(ctx)
            print(f"  动作: {decision.action.value}")
            print(f"  理由: {decision.reason}")

            # =========================================================
            # E - Execute（执行）
            # =========================================================
            if decision.action.value != "hold":
                print("[E] 执行交易...")
                place_tool = PlaceOrderTool(backend)
                ack = await place_tool.execute(decision)
                print(f"  订单 {ack.order_id} 状态: {ack.status}")
            else:
                print("[E] 持有不动")
                ack = {"order_id": "NONE", "status": "HOLD", "timestamp_ms": int(time.time()*1000)}

            # =========================================================
            # R - Record（记录）
            # =========================================================
            print("[R] 记录决策轨迹...")
            record = DecisionRecord(
                timestamp_ms=int(time.time() * 1000),
                observation=ctx.__dict__,
                analysis={
                    "thought": analysis.thought,
                    "market_assessment": analysis.market_assessment,
                    "risk_assessment": analysis.risk_assessment,
                    "confidence": analysis.confidence,
                },
                decision={
                    "action": decision.action.value,
                    "symbol": decision.symbol,
                    "quantity": decision.quantity,
                    "reason": decision.reason,
                    "confidence": decision.confidence,
                },
                execution=ack,
                pnl=0.0,  # 真实场景需从后端查询
            )
            recorder.record(record)

        except Exception as e:
            print(f"[ERROR] 循环异常: {e}")
            # 异常时触发 RL 降级（模式 3）
            fallback = RLFallbackAgent(llm, rl)
            decision = await fallback._rl_decide(ctx)
            print(f"[FALLBACK] RL 降级决策: {decision.action.value}")

        # 控制循环频率
        elapsed = time.perf_counter() - loop_start
        sleep_time = max(0, interval_seconds - elapsed)
        if sleep_time > 0:
            print(f"  等待 {sleep_time:.1f}s 进入下一轮...")
            await asyncio.sleep(sleep_time)

    # 循环结束，生成报告
    print("\n" + "=" * 60)
    print("OADER 循环结束，生成报告")
    print("=" * 60)
    report = bridge.generate_report()
    print(report)


async def main() -> int:
    """入口函数。"""
    await oader_loop(
        symbol="BTC-USDT",
        max_iterations=5,        # 演示运行 5 轮
        interval_seconds=2.0,    # 每 2 秒一轮（生产环境建议 60s+）
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
```

---

## 参考源码路径

- `crates/axon-llm/src/agent.rs` — `Agent::run_reasoning_cycle()` ReAct 主循环
- `crates/axon-llm/src/context.rs` — `ContextBuilder` 观察上下文构建
- `crates/axon-llm/src/prompt.rs` — `SystemPrompt` 提示词模板
- `crates/axon-llm/src/tools.rs` — `ToolRegistry` 工具注册表
- `crates/axon-llm/src/trading/place_order_tool.rs` — `PlaceOrderTool` 下单工具
- `crates/axon-llm/src/trading/query_portfolio_tool.rs` — `QueryPortfolioTool` 持仓查询
- `crates/axon-llm/src/trading/safety.rs` — `SafetyGuard` 安全围栏
- `crates/axon-llm/src/trading/backend.rs` — `TradingBackend` trait 定义
- `crates/axon-llm/src/trading/mock.rs` — `MockTradingBackend` 回测后端
- `crates/axon-llm/src/explain/recorder.rs` — `ExplainRecorder` 决策记录器
- `crates/axon-llm/src/explain/store.rs` — `ExplainStore` 持久化存储
- `crates/axon-llm/src/explain/bridge.rs` — `ExplainBridge` 报告生成
- `crates/axon-llm/src/explain/tools.rs` — `ExplainTools` 可解释性工具
- `crates/axon-llm/src/backends/openai_compat.rs` — `OpenAICompatBackend` LLM 后端
- `crates/axon-llm/src/backends/mock.rs` — `MockBackend` 测试后端
- `crates/axon-llm/src/config.rs` — `AgentConfig` 智能体配置
- `crates/axon-rl/src/env/trading_env.rs` — `TradingEnv` RL 环境
- `crates/axon-backtest/src/engine.rs` — `BacktestEngine` 回测引擎
- `crates/axon-inference/src/backend/onnx.rs` — `OnnxBackend` 推理后端
