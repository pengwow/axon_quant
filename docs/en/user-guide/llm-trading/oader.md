# Scenario 2 — LLM Agent Trading (OADER Loop)

> **Full example**: [`examples/13_llm/llm_demo.py`](../../../examples/13_llm/llm_demo.py)
> LLM + Trading agent demo: market analysis → signal generation → risk control → Mock execution → ReAct loop.

This document provides an in-depth analysis of the core trading loop for LLM agents in the AXON quantitative platform: the **OADER model** (Observe-Analyze-Decide-Execute-Record). OADER combines the ReAct (Reasoning + Acting) reasoning paradigm with rigorous quantitative trading risk control, supporting both live trading and backtesting modes. All code examples are based on AXON `0.1.0` real source code.

---

## OADER Model Introduction

OADER is AXON's five-stage closed-loop model designed for LLM-driven quantitative trading, named after the first letters of the five core stages:

| Stage | English | Responsibility | Source Module |
|-------|---------|----------------|---------------|
| O | **Observe** | Collect market data, portfolio snapshots, strategy state | `axon-llm/src/context.rs` |
| A | **Analyze** | LLM reasoning: understand market conditions, generate trading ideas | `axon-llm/src/agent.rs` |
| D | **Decide** | Based on analysis, determine trading actions (buy/sell/hold) | `axon-llm/src/agent.rs` |
| E | **Execute** | Call trading tools to place orders, query portfolio | `axon-llm/src/trading/` |
| R | **Record** | Record decision trajectory, write back context, generate explainability reports | `axon-llm/src/explain/` |

Each stage of OADER has clear data contracts and safety boundaries, ensuring LLM's "creativity" does not breach risk control limits.

---

## OADER Five Stages in Detail

### Architecture Overview

```text
+------------------------------------------------------------------+
|                         OADER Trading Loop                        |
+------------------------------------------------------------------+
|                                                                  |
|  +-----------+   +-----------+   +-----------+   +-----------+  |
|  |  Observe  |-->|  Analyze  |-->|  Decide   |-->|  Execute  |  |
|  +-----------+   +-----------+   +-----------+   +-----------+  |
|       ^                                              |           |
|       |                                              v           |
|       |                                        +-----------+     |
|       |                                        |  Record   |     |
|       |                                        +-----------+     |
|       |                                              |           |
|       +----------------------------------------------+           |
|                        (Context Writeback)                       |
+------------------------------------------------------------------+
```

### Stage 1: Observe

**Responsibility**: Collect all context information available for LLM decision-making.
**Source**: `crates/axon-llm/src/context.rs`

`ContextBuilder` assembles three types of input:

1. **Market Data**: Current K-line, order book, technical indicators (via `MarketDataTool`)
2. **Portfolio Snapshot**: Current balance, position list, floating PnL (via `QueryPortfolioTool`)
3. **Strategy State**: Previous decision records, cumulative PnL, runtime (via `ExplainRecorder` context writeback)

```python
"""
Observe Stage: Build LLM Decision Context
Source: crates/axon-llm/src/context.rs
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class ObservationContext:
    """OADER Observe stage output data structure."""
    # Market data: Current market snapshot
    market_data: dict[str, Any] = field(default_factory=dict)
    # Portfolio snapshot: Balance + positions list
    portfolio: dict[str, Any] = field(default_factory=dict)
    # Strategy state: Previous decisions, cumulative PnL, etc.
    strategy_state: dict[str, Any] = field(default_factory=dict)
    # Timestamp (milliseconds)
    timestamp_ms: int = 0


class ContextBuilder:
    """
    Context builder: Aggregates multiple data sources into ObservationContext for LLM.
    Corresponds to Rust ContextBuilder trait implementation.
    """

    def __init__(self):
        self._market_data_tool = None   # MarketDataTool
        self._portfolio_tool = None     # QueryPortfolioTool
        self._recorder = None           # ExplainRecorder (for reading historical state)

    def with_market_data(self, symbol: str, timeframe: str = "1h") -> "ContextBuilder":
        """Inject market data tool to get quotes for specified trading pair."""
        self._market_data_tool = {"symbol": symbol, "timeframe": timeframe}
        return self

    def with_portfolio(self) -> "ContextBuilder":
        """Inject portfolio query tool."""
        self._portfolio_tool = {"type": "QueryPortfolio"}
        return self

    def with_strategy_state(self, recorder) -> "ContextBuilder":
        """Inject strategy state recorder to read previous decision history."""
        self._recorder = recorder
        return self

    def build(self) -> ObservationContext:
        """Assemble complete observation context."""
        ctx = ObservationContext()

        # 1. Collect market data
        if self._market_data_tool:
            ctx.market_data = {
                "symbol": self._market_data_tool["symbol"],
                "price": 50_000.0,          # Simulated current price
                "change_24h": 0.025,        # 24h price change
                "volume_24h": 1_200_000_000.0,
            }

        # 2. Collect portfolio snapshot
        if self._portfolio_tool:
            ctx.portfolio = {
                "balance": {"USDT": 10_000.0, "BTC": 0.0},
                "positions": [],             # No current positions
            }

        # 3. Read strategy state (previous decision records)
        if self._recorder:
            ctx.strategy_state = self._recorder.get_last_state()

        import time
        ctx.timestamp_ms = int(time.time() * 1000)
        return ctx


# Usage example
if __name__ == "__main__":
    builder = ContextBuilder()
    ctx = (
        builder
        .with_market_data("BTC-USDT", timeframe="1h")
        .with_portfolio()
        .build()
    )
    print(f"[Observe] Context built: {ctx}")
```

### Stage 2: Analyze

**Responsibility**: LLM reasons based on observed context, generating trading analysis ideas.
**Source**: `crates/axon-llm/src/agent.rs` (`run_reasoning_cycle`)

The analysis stage is the core of the ReAct loop. LLM receives system prompt (`SystemPrompt`) + observation context, outputs structured `AnalysisResult` containing:

- `thought`: Internal reasoning process (explainability)
- `market_assessment`: Market condition assessment (trend/range/reversal)
- `risk_assessment`: Risk level (low/medium/high)
- `confidence`: Confidence score (0.0 ~ 1.0)

```python
"""
Analyze Stage: LLM Reasoning and Market Analysis
Source: crates/axon-llm/src/agent.rs run_reasoning_cycle
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass
class AnalysisResult:
    """Analyze stage output."""
    thought: str                    # LLM's internal reasoning process
    market_assessment: str          # Market condition: "uptrend" / "downtrend" / "range"
    risk_assessment: str            # Risk level: "low" / "medium" / "high"
    confidence: float               # Confidence 0.0~1.0
    reasoning_steps: list[str]      # ReAct step-by-step reasoning chain


class Analyzer:
    """
    Analyzer: Calls LLM for market analysis.
    Corresponds to Rust Agent::run_reasoning_cycle method.
    """

    def __init__(self, backend):
        """
        backend: LLM backend (OpenAICompatBackend / MockBackend)
        """
        self.backend = backend

    def analyze(self, ctx: "ObservationContext") -> AnalysisResult:
        """
        Execute analysis reasoning.
        In Rust, this corresponds to:
            let response = self.backend.complete(prompt).await?;
        """
        # Build system prompt (corresponds to SystemPrompt::new)
        system_prompt = (
            "You are a quantitative trading analyst. "
            "Analyze the provided market data and portfolio state. "
            "Output your reasoning in structured JSON."
        )

        # Build user prompt (containing all ObservationContext information)
        user_prompt = self._format_context(ctx)

        # Call LLM backend (simulated)
        raw_response = self.backend.complete(system_prompt, user_prompt)

        # Parse structured output
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
        """Format ObservationContext into LLM-readable text."""
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


# Usage example
if __name__ == "__main__":
    class MockBackend:
        def complete(self, system: str, user: str) -> str:
            return "mock_response"

    analyzer = Analyzer(MockBackend())
    # Assume ObservationContext exists
    # result = analyzer.analyze(ctx)
    print("[Analyze] Analyzer initialized")
```

### Stage 3: Decide

**Responsibility**: Based on analysis results, output final trading decisions.
**Source**: `crates/axon-llm/src/agent.rs` (Decide branch of `run_reasoning_cycle`)

The decision stage maps `AnalysisResult` to specific trading actions. AXON supports three decision modes:

1. **LLM Direct Decision**: LLM outputs `action` field (Buy / Sell / Hold)
2. **RL-Assisted Decision**: RL model provides action probabilities, LLM modifies based on this
3. **Rule-Based Fallback**: When confidence is below threshold, triggers preset rule strategy

```python
"""
Decide Stage: Trading Decision
Source: crates/axon-llm/src/agent.rs decision logic
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any, Optional


class ActionType(Enum):
    """Trading action type."""
    BUY = "buy"
    SELL = "sell"
    HOLD = "hold"


@dataclass
class Decision:
    """Decide stage output."""
    action: ActionType              # Trading action
    symbol: str                     # Trading pair
    quantity: Optional[float]       # Quantity (None means calculated by risk module)
    order_type: str                 # "limit" / "market"
    price: Optional[float]          # Limit order price
    stop_loss: Optional[float]      # Stop loss price
    take_profit: Optional[float]    # Take profit price
    reason: str                     # Decision reason (explainability)
    confidence: float               # Decision confidence


class DecisionEngine:
    """
    Decision engine: Converts analysis results to specific trading instructions.
    Corresponds to Rust Agent decision branch.
    """

    # Minimum confidence threshold: triggers HOLD below this
    MIN_CONFIDENCE = 0.6

    def __init__(self, risk_limits: dict[str, Any] | None = None):
        self.risk_limits = risk_limits or {
            "max_position_size": 0.1,      # Maximum position 10%
            "max_drawdown_pct": 0.05,      # Maximum drawdown 5%
        }

    def decide(self, analysis: "AnalysisResult", ctx: "ObservationContext") -> Decision:
        """
        Generate trading decision based on analysis results.
        Safety mechanism: Force HOLD when confidence is insufficient.
        """
        # Safety mechanism 1: Confidence check
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

        # Safety mechanism 2: Risk assessment check
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

        # Normal decision logic
        symbol = ctx.market_data.get("symbol", "BTC-USDT")
        price = ctx.market_data.get("price", 50_000.0)

        if analysis.market_assessment == "uptrend":
            return Decision(
                action=ActionType.BUY,
                symbol=symbol,
                quantity=0.01,              # Example fixed quantity, real scenario uses risk calculation
                order_type="limit",
                price=price * 0.995,        # Slightly below market price limit order
                stop_loss=price * 0.95,     # 5% stop loss
                take_profit=price * 1.05,   # 5% take profit
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


# Usage example
if __name__ == "__main__":
    engine = DecisionEngine()
    print("[Decide] Decision engine initialized")
```

### Stage 4: Execute

**Responsibility**: Convert decisions to actual trading operations, call exchange API or backtesting engine.
**Source**: `crates/axon-llm/src/trading/place_order_tool.rs`, `query_portfolio_tool.rs`

The execution stage interacts with the trading backend via `PlaceOrderTool` and `QueryPortfolioTool`. AXON supports two execution modes:

- **Live Mode**: Calls real exchange API (via `TradingBackend` trait)
- **Backtest Mode**: Calls `BacktestEngine`'s `step()` method (via `BacktestTradingBackend`)

```python
"""
Execute Stage: Trading Execution
Source:
  - crates/axon-llm/src/trading/place_order_tool.rs
  - crates/axon-llm/src/trading/query_portfolio_tool.rs
  - crates/axon-llm/src/trading/backend.rs
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Optional


@dataclass
class OrderAck:
    """Order acknowledgment (corresponds to Rust OrderAck)."""
    order_id: str
    symbol: str
    side: str
    quantity: float
    status: str
    timestamp_ms: int
    confirm_token: Optional[str] = None


class PlaceOrderTool:
    """
    Order placement tool: Converts Decision to exchange order.
    Corresponds to Rust PlaceOrderTool trait implementation.
    """

    def __init__(self, backend: Any):
        """
        backend: TradingBackend instance (live or backtest)
        """
        self.backend = backend

    async def execute(self, decision: "Decision") -> OrderAck:
        """
        Execute trading decision.
        In Rust:
            let ack = self.backend.place_order(args).await?;
        """
        # Build order parameters (corresponds to PlaceOrderArgs)
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

        # Call backend to execute
        ack = await self.backend.place_order(order_args)
        return ack


class QueryPortfolioTool:
    """
    Portfolio query tool: Get current investment portfolio state.
    Corresponds to Rust QueryPortfolioTool.
    """

    def __init__(self, backend: Any):
        self.backend = backend

    async def query(self, symbol: Optional[str] = None) -> dict[str, Any]:
        """
        Query portfolio.
        In Rust:
            let snapshot = self.backend.query_portfolio(args).await?;
        """
        args = {"symbol": symbol}
        return await self.backend.query_portfolio(args)


# Usage example
if __name__ == "__main__":
    print("[Execute] Trading execution tools initialized")
```

### Stage 5: Record

**Responsibility**: Record complete decision trajectory, support explainability analysis and strategy iteration.
**Source**: `crates/axon-llm/src/explain/recorder.rs`, `store.rs`

The recording stage is key to the OADER closed loop. `ExplainRecorder` persists each loop's context, analysis, decisions, and execution results to `ExplainStore`, and generates structured reports via `ExplainBridge`.

```python
"""
Record Stage: Decision Trajectory Recording and Explainability
Source:
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
    """Complete record of a single OADER loop."""
    timestamp_ms: int
    observation: dict[str, Any]      # Observe stage original input
    analysis: dict[str, Any]         # Analyze stage output
    decision: dict[str, Any]         # Decide stage output
    execution: dict[str, Any]        # Execute stage acknowledgment
    pnl: float = 0.0                 # This step's PnL


class ExplainRecorder:
    """
    Decision recorder: Records complete trajectory of each OADER loop.
    Corresponds to Rust ExplainRecorder.
    """

    def __init__(self, store: "ExplainStore"):
        self.store = store
        self._records: list[DecisionRecord] = []

    def record(self, record: DecisionRecord) -> None:
        """Record a decision loop."""
        self._records.append(record)
        # Persist to storage
        self.store.append(record)

    def get_last_state(self) -> dict[str, Any]:
        """Get previous step's strategy state (for next Observe)."""
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
        """Get all records."""
        return self._records.copy()


class ExplainStore:
    """
    Decision storage: Persists decision records.
    Corresponds to Rust ExplainStore.
    """

    def __init__(self, path: str = "explain_store.json"):
        self.path = path
        self._data: list[dict[str, Any]] = []

    def append(self, record: DecisionRecord) -> None:
        """Append record."""
        self._data.append({
            "timestamp_ms": record.timestamp_ms,
            "observation": record.observation,
            "analysis": record.analysis,
            "decision": record.decision,
            "execution": record.execution,
            "pnl": record.pnl,
        })

    def query(self, start_ms: int, end_ms: int) -> list[dict[str, Any]]:
        """Query records by time range."""
        return [r for r in self._data if start_ms <= r["timestamp_ms"] <= end_ms]


class ExplainBridge:
    """
    Explainability bridge: Converts records to human-readable reports.
    Corresponds to Rust ExplainBridge.
    """

    def __init__(self, recorder: ExplainRecorder):
        self.recorder = recorder

    def generate_report(self) -> str:
        """Generate explainability report."""
        records = self.recorder.get_records()
        lines = [
            "# OADER Trading Decision Report",
            f"Total Steps: {len(records)}",
            f"Total PnL: {sum(r.pnl for r in records):.2f}",
            "",
            "## Decision Details",
        ]
        for i, r in enumerate(records, 1):
            lines.append(f"### Step {i}")
            lines.append(f"- Action: {r.decision.get('action', 'N/A')}")
            lines.append(f"- Reason: {r.decision.get('reason', 'N/A')}")
            lines.append(f"- PnL: {r.pnl:.2f}")
            lines.append("")
        return "\n".join(lines)


# Usage example
if __name__ == "__main__":
    store = ExplainStore()
    recorder = ExplainRecorder(store)
    print("[Record] Recording system initialized")
```

---

## ReAct Reasoning Loop Core Logic

### ReAct Mapping in OADER

ReAct (Reasoning + Acting) is the core reasoning paradigm in the OADER Analyze stage. AXON adapts the classic ReAct loop for quantitative trading scenarios:

```text
+-------------------------------------------------------------+
|                    ReAct Reasoning Loop                       |
+-------------------------------------------------------------+
|                                                             |
|   +------------+    +------------+    +------------+       |
|   |  Thought   | -> |   Action   | -> | Observation|       |
|   +------------+    +------------+    +------------+       |
|        ^                                    |               |
|        |                                    v               |
|        +------------------------------------+               |
|                    (Loop Iteration)                          |
+-------------------------------------------------------------+
```

### Four Key Mechanisms

#### Mechanism 1: Structured Prompts (System Prompt)

AXON forces LLM to output structured JSON via `SystemPrompt`, ensuring downstream modules can parse:

```python
"""
ReAct Mechanism 1: Structured Prompts
Source: crates/axon-llm/src/prompt.rs
"""

from __future__ import annotations


class SystemPrompt:
    """
    System prompt template: Constrains LLM output format.
    Corresponds to Rust SystemPrompt::new.
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
        """Build system prompt."""
        prompt = cls.TEMPLATE
        if extra_rules:
            prompt += "\nAdditional Rules:\n" + "\n".join(f"- {r}" for r in extra_rules)
        return prompt


# Usage example
if __name__ == "__main__":
    prompt = SystemPrompt.build(["Max position size: 10%", "Stop loss required for all trades"])
    print("[ReAct] System prompt built")
```

#### Mechanism 2: Tool Use

AXON's LLM tool system allows agents to call external tools for real-time data during reasoning:

```python
"""
ReAct Mechanism 2: Tool Use System
Source: crates/axon-llm/src/tools.rs
"""

from __future__ import annotations

from typing import Any, Callable


class Tool:
    """Tool definition: Corresponds to Rust Tool trait."""

    def __init__(self, name: str, description: str, handler: Callable[..., Any]):
        self.name = name
        self.description = description
        self.handler = handler

    def call(self, **kwargs: Any) -> Any:
        """Execute tool."""
        return self.handler(**kwargs)


class ToolRegistry:
    """
    Tool registry: Manages all available tools.
    Corresponds to Rust ToolRegistry.
    """

    def __init__(self):
        self._tools: dict[str, Tool] = {}

    def register(self, tool: Tool) -> "ToolRegistry":
        """Register tool."""
        self._tools[tool.name] = tool
        return self

    def get(self, name: str) -> Tool:
        """Get tool."""
        return self._tools[name]

    def list_tools(self) -> list[str]:
        """List all tool names."""
        return list(self._tools.keys())

    def build_tool_description(self) -> str:
        """
        Build tool description text for LLM to understand available tools.
        Corresponds to Rust tool description generation logic.
        """
        lines = ["Available Tools:"]
        for name, tool in self._tools.items():
            lines.append(f"- {name}: {tool.description}")
        return "\n".join(lines)


# Usage example
if __name__ == "__main__":
    registry = ToolRegistry()
    registry.register(Tool(
        name="get_market_data",
        description="Get real-time market data for specified trading pair",
        handler=lambda symbol, timeframe: {"price": 50000, "change": 0.02},
    ))
    registry.register(Tool(
        name="get_portfolio",
        description="Query current investment portfolio state",
        handler=lambda: {"balance": 10000, "positions": []},
    ))
    print(f"[ReAct] Registered tools: {registry.list_tools()}")
```

#### Mechanism 3: Chain-of-Thought Tracing

AXON records LLM's every reasoning step via `ExplainRecorder`, forming a complete decision audit chain:

```python
"""
ReAct Mechanism 3: Chain-of-Thought Tracing
Source: crates/axon-llm/src/explain/recorder.rs
"""

from __future__ import annotations

from typing import Any


class ChainOfThoughtTracer:
    """
    Chain-of-thought tracer: Records every thinking step in ReAct loop.
    Corresponds to Rust ExplainRecorder's reasoning_steps field.
    """

    def __init__(self):
        self._steps: list[dict[str, Any]] = []

    def add_thought(self, step: int, thought: str, action: str, observation: str) -> None:
        """Record a ReAct loop step."""
        self._steps.append({
            "step": step,
            "thought": thought,
            "action": action,
            "observation": observation,
        })

    def get_chain(self) -> list[dict[str, Any]]:
        """Get complete reasoning chain."""
        return self._steps.copy()

    def format_chain(self) -> str:
        """Format reasoning chain as human-readable text."""
        lines = ["## ReAct Reasoning Chain"]
        for s in self._steps:
            lines.append(f"### Step {s['step']}")
            lines.append(f"**Thought**: {s['thought']}")
            lines.append(f"**Action**: {s['action']}")
            lines.append(f"**Observation**: {s['observation']}")
            lines.append("")
        return "\n".join(lines)


# Usage example
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

#### Mechanism 4: Safety Guardrails

AXON embeds multi-layer safety mechanisms in the ReAct loop to prevent LLM from making dangerous decisions:

```python
"""
ReAct Mechanism 4: Safety Guardrails
Source: crates/axon-llm/src/trading/safety.rs
"""

from __future__ import annotations

from typing import Any


class SafetyGuard:
    """
    Safety guardrails: Performs multi-dimensional safety checks before decision execution.
    Corresponds to Rust SafetyGuard.
    """

    def __init__(self, limits: dict[str, Any] | None = None):
        self.limits = limits or {
            "max_order_size": 1.0,           # Maximum single order size
            "max_daily_orders": 10,          # Maximum daily orders
            "max_position_value_usd": 5000,  # Maximum position value
            "forbidden_symbols": ["MEME"],   # Forbidden trading pairs
        }
        self._daily_order_count = 0

    def check(self, decision: "Decision", portfolio: dict[str, Any]) -> tuple[bool, str]:
        """
        Safety check: Returns (passed, rejection reason).
        Corresponds to Rust SafetyGuard::check.
        """
        # Check 1: Forbidden trading pairs
        if decision.symbol in self.limits["forbidden_symbols"]:
            return False, f"Symbol {decision.symbol} is in forbidden list"

        # Check 2: Maximum order size
        if decision.quantity and decision.quantity > self.limits["max_order_size"]:
            return False, f"Order size {decision.quantity} exceeds limit {self.limits['max_order_size']}"

        # Check 3: Daily order limit
        if self._daily_order_count >= self.limits["max_daily_orders"]:
            return False, f"Daily order limit {self.limits['max_daily_orders']} reached"

        # Check 4: Position value limit
        if decision.action.value == "buy":
            current_value = portfolio.get("total_value", 0)
            order_value = (decision.quantity or 0) * (decision.price or 0)
            if current_value + order_value > self.limits["max_position_value_usd"]:
                return False, "Position value would exceed limit"

        self._daily_order_count += 1
        return True, ""


# Usage example
if __name__ == "__main__":
    guard = SafetyGuard()
    print("[ReAct] Safety guardrails initialized")
```

---

## Multi-Model Collaborative Decision Table

AXON's OADER loop supports multiple LLM backends working collaboratively, with different models taking different roles:

| Model Type | Role in OADER | Typical Use Case | Source |
|-----------|--------------|-----------------|--------|
| **Large Language Model (LLM)** | Analyze + Decide main reasoning engine | Market analysis, strategy reasoning, decision generation | `axon-llm/src/agent.rs` |
| **Embedding Model** | Observe stage semantic retrieval | Retrieving historical similar market conditions, strategy matching | `axon-llm/src/context.rs` |
| **RL Strategy Model** | Decide stage action recommendation | Provides action probability distribution for LLM reference | `axon-rl/src/env/trading_env.rs` |
| **Time Series Prediction Model** | Observe stage feature enhancement | Generates price predictions, volatility estimates | `axon-data/src/features.rs` |
| **Risk Control Rule Engine** | Execute stage pre-check | Position limits, stop loss checks, compliance review | `axon-llm/src/trading/safety.rs` |

### Multi-Model Collaboration Code Example

```python
"""
Multi-Model Collaborative Decision Example
Demonstrates integrating LLM + RL + risk control models in OADER loop
"""

from __future__ import annotations

from typing import Any


class MultiModelOrchestrator:
    """
    Multi-model orchestrator: Coordinates LLM, RL, and risk control models for joint decision-making.
    """

    def __init__(
        self,
        llm_backend: Any,      # LLM backend (OpenAI / Local)
        rl_model: Any,         # RL strategy model (PPO / SAC)
        safety_guard: Any,     # Risk control rule engine
    ):
        self.llm = llm_backend
        self.rl = rl_model
        self.safety = safety_guard

    async def decide(self, ctx: "ObservationContext") -> "Decision":
        """
        Multi-model collaborative decision flow:
        1. RL model provides action probabilities
        2. LLM makes final decision based on RL output + market context
        3. Risk control engine performs final check
        """
        # Step 1: RL model recommendation
        rl_action, rl_probs = self.rl.predict(ctx.market_data)

        # Step 2: LLM comprehensive decision (input includes RL recommendation)
        llm_input = {
            **ctx.__dict__,
            "rl_recommendation": rl_action,
            "rl_confidence": max(rl_probs),
        }
        analysis = await self.llm.analyze(llm_input)

        # Step 3: Risk control check
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


# Usage example
if __name__ == "__main__":
    print("[Multi-Model] Orchestrator initialized")
```

---

## ReAct and Backtesting Integration

### Live vs Backtest Comparison Table

| Dimension | Live Mode | Backtest Mode | Switching Method |
|-----------|-----------|---------------|-----------------|
| **Trading Backend** | `LiveTradingBackend` (calls exchange API) | `BacktestTradingBackend` (calls `BacktestEngine.step()`) | Polymorphic switching via `TradingBackend` trait |
| **Data Latency** | Real network latency | Zero latency (simulated clock advancement) | `SimulatedClock` vs system clock |
| **Order Execution** | Real matching (L1/L2/L3) | Simulated matching (L1MatchingEngine) | `MatchingEngine` trait implementation |
| **Portfolio Query** | Exchange API | Backtesting engine internal state | `QueryPortfolioTool` unified interface |
| **ExplainStore** | Writes to production database | Writes to temp file/memory | `ExplainStore` trait implementation |
| **Safety Mechanisms** | All enabled (including capital limits) | Some limits can be relaxed for stress testing | `SafetyGuard` configuration parameters |

### Backtest Mode Code Example

```python
"""
ReAct and Backtesting Integration Example
Demonstrates running complete OADER loop in backtest mode
Source: crates/axon-llm/src/trading/backend.rs
"""

from __future__ import annotations

import asyncio
from typing import Any


class BacktestTradingBackend:
    """
    Backtesting trading backend: Maps OADER Execute stage to BacktestEngine.
    Corresponds to Rust BacktestTradingBackend.
    """

    def __init__(self, engine: Any):
        """
        engine: BacktestEngine instance
        """
        self.engine = engine
        self._order_id_counter = 0

    async def place_order(self, args: dict[str, Any]) -> dict[str, Any]:
        """
        Simulate order placement in backtesting engine.
        In Rust:
            let event = Event::new_order_submitted(...);
            engine.step(event);
        """
        self._order_id_counter += 1
        order_id = f"BT-{self._order_id_counter}"

        # Build order submission event, push to backtesting engine
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

        # Step backtesting engine
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
        """Query backtesting engine's internal portfolio state."""
        return self.engine.get_portfolio_snapshot()


async def run_backtest_oader_loop():
    """Run complete OADER loop in backtest mode."""
    # Initialize backtesting engine (corresponds to step 5 backtest configuration)
    engine = {
        "current_timestamp": lambda: 1_700_000_000_000,
        "step": lambda e: {"pnl": 0.0},
        "get_portfolio_snapshot": lambda: {"balance": {"USDT": 10000}, "positions": []},
    }

    backend = BacktestTradingBackend(engine)
    place_order_tool = PlaceOrderTool(backend)
    query_portfolio_tool = QueryPortfolioTool(backend)

    # Run 10 steps of OADER loop
    for step in range(10):
        # Observe
        ctx = ContextBuilder().with_market_data("BTC-USDT").with_portfolio().build()

        # Analyze (simplified: directly generate decision)
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
            print(f"[Backtest] Step {step}: Order {ack['order_id']} status={ack['status']}")

        # Record
        print(f"[Backtest] Step {step}: Complete")


if __name__ == "__main__":
    asyncio.run(run_backtest_oader_loop())
```

---

## ReAct and HPO Integration

### Integration Pipeline Diagram

```text
+----------------+     +----------------+     +----------------+
|   HPO Search   | --> |  RL Training   | --> |  ReAct Call    |
| (OptunaStudy)  |     | (PPO+TradingEnv)|     | (OADER Loop)   |
+----------------+     +----------------+     +----------------+
        |                       |                       |
        v                       v                       v
  Search space            Train strategy         Evaluate strategy
  (lr, gamma,             (model.zip)            (Sharpe / PnL)
   batch_size)
        |                       |                       |
        +-----------------------+-----------------------+
                                |
                                v
                        +----------------+
                        |  Feedback to   |
                        |  HPO (objective|
                        |  function      |
                        |  scoring)      |
                        +----------------+
```

### HPO → RL → ReAct Code Example

```python
"""
ReAct and HPO Integration Example
Demonstrates using OADER loop performance as HPO objective function
"""

from __future__ import annotations

import asyncio
import json
from typing import Any

import axon_quant

hpo = axon_quant.hpo


async def evaluate_react_strategy(params: dict[str, Any]) -> list[float]:
    """
    HPO objective function: Train RL model with a set of hyperparameters,
    then evaluate in ReAct loop.
    Returns: [sharpe_ratio, -max_drawdown]
    """
    # 1. Train RL model with current trial's hyperparameters
    # (reuses step 2 training logic)
    lr = params["learning_rate"]
    gamma = params["gamma"]
    print(f"[HPO→ReAct] Training RL model: lr={lr}, gamma={gamma}")

    # 2. Integrate trained RL model into OADER Decide stage
    # rl_model = PPO.load(f"models/trial_{trial_id}.zip")

    # 3. Run ReAct backtest loop (as shown in previous section)
    # Collect 100 steps of PnL series
    pnl_series = [0.01, -0.005, 0.015, -0.002, 0.008] * 20  # Simulated

    # 4. Calculate performance metrics
    import numpy as np
    returns = np.array(pnl_series)
    sharpe = np.mean(returns) / (np.std(returns) + 1e-9) * np.sqrt(252)
    cumulative = np.cumsum(returns)
    max_dd = np.max(np.maximum.accumulate(cumulative) - cumulative)

    print(f"[HPO→ReAct] Evaluation results: Sharpe={sharpe:.3f}, MaxDD={max_dd:.3f}")
    return [sharpe, -max_dd]


def main() -> int:
    print("=" * 60)
    print("HPO → RL → ReAct Integration Example")
    print("=" * 60)

    # Define search space
    search_space = {
        "learning_rate": hpo.SearchSpaceDef(param_type="log_uniform", low=1e-5, high=1e-3),
        "gamma": hpo.SearchSpaceDef(param_type="uniform", low=0.95, high=0.999),
    }

    # Create HPO runner (note: objective function needs sync wrapper because Optuna doesn't support async)
    def sync_objective(params):
        return asyncio.run(evaluate_react_strategy(params))

    runner = hpo.OptunaHPO(
        search_space=search_space,
        objective_fn=sync_objective,
        study_name="react_rl_hpo",
        directions=["maximize", "maximize"],
        sampler=hpo.SamplerConfig(sampler_type="tpe", seed=42),
    )

    # Execute search
    results = runner.run(n_trials=10, n_jobs=1)
    print(f"\n[HPO→ReAct] Completed {len(results)} trials")

    best = runner.get_best_trial()
    if best:
        print(f"[HPO→ReAct] Best hyperparameters: {best.params}")
        with open("best_react_hpo.json", "w") as f:
            json.dump(best.params, f, indent=2)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

---

## ReAct and RL Integration

AXON supports three ReAct and RL collaboration modes, covering the full spectrum from "LLM-led" to "RL-led":

### Mode 1: RL-Augmented LLM

**Description**: RL model provides action probability distribution, LLM uses it as one reference signal combined with its own reasoning for final decision.
**Use Case**: Complex market structure, need LLM to understand unstructured information (news, sentiment).

```python
"""
Mode 1: RL-Augmented LLM
RL provides action probabilities, LLM makes final decision
"""

from __future__ import annotations

from typing import Any
import numpy as np


class RLAugmentedLLM:
    """RL-augmented LLM decision maker."""

    def __init__(self, llm_backend: Any, rl_model: Any, rl_weight: float = 0.3):
        self.llm = llm_backend
        self.rl = rl_model
        self.rl_weight = rl_weight  # RL signal weight

    async def decide(self, ctx: "ObservationContext") -> "Decision":
        # 1. LLM independent analysis
        llm_analysis = await self.llm.analyze(ctx)

        # 2. RL model outputs action probabilities
        obs = self._extract_observation(ctx)
        rl_action, rl_probs = self.rl.predict(obs)

        # 3. Fused decision: LLM confidence weighted with RL probabilities
        llm_confidence = llm_analysis.confidence
        rl_confidence = float(np.max(rl_probs))

        # If RL has strong signal and LLM is uncertain, boost confidence
        if rl_confidence > 0.8 and llm_confidence < 0.6:
            fused_confidence = llm_confidence * (1 - self.rl_weight) + rl_confidence * self.rl_weight
            llm_analysis.confidence = min(fused_confidence, 0.95)
            llm_analysis.reasoning_steps.append(
                f"RL signal boosted confidence: {rl_action} (prob={rl_confidence:.2f})"
            )

        return DecisionEngine().decide(llm_analysis, ctx)

    def _extract_observation(self, ctx: "ObservationContext") -> np.ndarray:
        """Convert ObservationContext to RL model observation vector."""
        return np.array([
            ctx.market_data.get("price", 0),
            ctx.market_data.get("change_24h", 0),
            ctx.portfolio.get("balance", {}).get("USDT", 0),
        ])


# Usage example
if __name__ == "__main__":
    print("[RL+LLM] Mode 1: RL-Augmented LLM initialized")
```

### Mode 2: LLM-Guided RL

**Description**: LLM generates reward shaping signals or curriculum learning targets to guide RL model faster convergence.
**Use Case**: RL training initial exploration is inefficient, need LLM to provide prior knowledge.

```python
"""
Mode 2: LLM-Guided RL
LLM generates reward shaping signals to guide RL policy learning
"""

from __future__ import annotations

from typing import Any


class LLMGuidedRL:
    """LLM-guided RL trainer."""

    def __init__(self, llm_backend: Any, base_reward: str = "pnl"):
        self.llm = llm_backend
        self.base_reward = base_reward

    def compute_shaped_reward(
        self,
        env_state: dict[str, Any],
        base_reward: float,
    ) -> float:
        """
        Compute shaped reward.
        LLM assesses current market state quality and adds shaping term.
        """
        # Let LLM assess current market state quality
        market_quality = self.llm.assess_market_quality(env_state)

        # If market quality is poor (high noise, low liquidity), reduce reward magnitude
        if market_quality == "poor":
            shaping_factor = 0.5
        elif market_quality == "good":
            shaping_factor = 1.2
        else:
            shaping_factor = 1.0

        shaped = base_reward * shaping_factor

        # Add LLM-guided exploration bonus
        if env_state.get("is_novel_state", False):
            exploration_bonus = 0.1
            shaped += exploration_bonus

        return shaped

    def generate_curriculum(self, performance_history: list[float]) -> list[dict[str, Any]]:
        """
        Generate curriculum learning targets.
        LLM decides next training difficulty based on historical performance.
        """
        avg_perf = sum(performance_history) / len(performance_history) if performance_history else 0

        if avg_perf < 0:
            # Poor performance: reduce difficulty, add stable trend data
            return [{"trend_strength": 0.8, "noise_level": 0.1}]
        elif avg_perf > 0.5:
            # Good performance: increase difficulty, introduce oscillation and reversal
            return [{"trend_strength": 0.3, "noise_level": 0.3, "reversal_prob": 0.2}]
        else:
            return [{"trend_strength": 0.5, "noise_level": 0.2}]


# Usage example
if __name__ == "__main__":
    print("[LLM→RL] Mode 2: LLM-Guided RL initialized")
```

### Mode 3: RL Fallback

**Description**: When LLM service is unavailable, response times out, or confidence is consistently low, automatically switch to pure RL strategy for trading.
**Use Case**: Production high-availability requirements, preventing LLM failures from causing trading interruptions.

```python
"""
Mode 3: RL Fallback
Automatically switch to RL strategy when LLM is unavailable
"""

from __future__ import annotations

import asyncio
from typing import Any


class RLFallbackAgent:
    """
    OADER agent with RL fallback.
    Corresponds to Rust Agent's fallback logic.
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
        Decision flow: Prefer LLM, fallback to RL on anomaly.
        """
        try:
            # Attempt LLM decision (with timeout)
            llm_task = asyncio.create_task(self._llm_decide(ctx))
            decision = await asyncio.wait_for(
                llm_task, timeout=self.fallback_timeout_ms / 1000
            )

            # Check LLM confidence
            if decision.confidence < self.min_confidence:
                print(f"[Fallback] LLM confidence {decision.confidence:.2f} too low, switching to RL")
                return await self._rl_decide(ctx)

            self._llm_count += 1
            return decision

        except asyncio.TimeoutError:
            print(f"[Fallback] LLM timeout ({self.fallback_timeout_ms}ms), switching to RL")
            self._fallback_count += 1
            return await self._rl_decide(ctx)
        except Exception as e:
            print(f"[Fallback] LLM exception: {e}, switching to RL")
            self._fallback_count += 1
            return await self._rl_decide(ctx)

    async def _llm_decide(self, ctx: "ObservationContext") -> "Decision":
        """LLM decision path."""
        analysis = await self.llm.analyze(ctx)
        return DecisionEngine().decide(analysis, ctx)

    async def _rl_decide(self, ctx: "ObservationContext") -> "Decision":
        """RL fallback decision path."""
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
            confidence=0.5,  # RL decisions default to medium confidence
        )

    def _extract_observation(self, ctx: "ObservationContext") -> Any:
        """Extract RL observation."""
        import numpy as np
        return np.array([
            ctx.market_data.get("price", 0),
            ctx.market_data.get("change_24h", 0),
        ])

    def get_stats(self) -> dict[str, int]:
        """Get decision statistics."""
        return {
            "llm_decisions": self._llm_count,
            "rl_fallbacks": self._fallback_count,
        }


# Usage example
if __name__ == "__main__":
    print("[RL Fallback] Mode 3: RL Fallback initialized")
```

---

## Security Mechanisms and Risk Isolation

AXON's security mechanisms are organized by OADER stages, forming a defense-in-depth system:

### Security Table by Stage

| OADER Stage | Security Mechanism | Implementation Location | Purpose |
|-------------|-------------------|------------------------|---------|
| **Observe** | Data source validation | `axon-data/src/validation.rs` | Prevents abnormal market data from entering decision flow |
| **Observe** | Context integrity check | `axon-llm/src/context.rs` | Ensures all required fields exist and are properly formatted |
| **Analyze** | Prompt injection filtering | `axon-llm/src/prompt.rs` | Prevents malicious input from contaminating LLM reasoning |
| **Analyze** | Output format validation | `axon-llm/src/agent.rs` | Enforces JSON Schema validation, rejects unstructured output |
| **Decide** | Confidence threshold | `DecisionEngine.MIN_CONFIDENCE` | Forces HOLD on low confidence |
| **Decide** | Risk assessment interception | `DecisionEngine.decide()` | Forces HOLD on high risk assessment |
| **Execute** | Safety guardrail check | `axon-llm/src/trading/safety.rs` | Position limits, forbidden trading pairs, daily order limits |
| **Execute** | Two-phase submission | `OrderAck.confirm_token` | Large orders require manual confirmation |
| **Execute** | Exchange API rate limiting | `TradingBackend` implementation | Prevents frequent calls from triggering exchange risk controls |
