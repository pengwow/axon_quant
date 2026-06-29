#!/usr/bin/env python3
"""AXON Harness Engineering 多智能体编排系统演示。

覆盖:
  1. HarnessBridge — 零侵入模式 vs 激活模式
  2. CircuitBreaker — 熔断器状态机（CLOSED → OPEN → HALF_OPEN → CLOSED）
  3. AuditChain — Blake3 哈希链（防篡改审计）
  4. DefaultPolicy — 默认裁决策略（置信度 + 预算区间）
  5. SimpleBudgetGuard — Token 预算守卫（区间转换）
  6. RBACToolGate — 基于角色的工具门控
  7. HarnessObserver — 可观测性组件（决策日志 + 性能指标）
  8. 状态机 — 7 态任务生命周期（纯 Python 实现）
  9. 快速路径路由器 — 三级决策路径（FAST / LIGHT / FULL）
  10. 投票共识 — 60% 同意通过，RiskAgent 一票否决
  11. 编排器 — 任务接收 → 路由 → LLM Agent 决策 → 投票 → 审查

运行方式:
    source .venv/bin/activate
    python examples/18_harness/harness_demo.py

环境变量（可选）:
    AXON_LLM_BASE_URL  — LLM API 地址（默认 https://api.openai.com/v1）
    AXON_LLM_API_KEY   — API Key
    AXON_LLM_MODEL     — 模型名（默认 gpt-4o-mini）
    AXON_STEP_DELAY     — 每步延迟秒数（默认 1.5）
"""

from __future__ import annotations

import json
import os
import sys
import time
import uuid
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable, Optional

# ─── ANSI 颜色 ──────────────────────────────────────────────────────────
RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
RED = "\033[31m"
GREEN = "\033[32m"
YELLOW = "\033[33m"
CYAN = "\033[36m"
MAGENTA = "\033[35m"
WHITE = "\033[97m"

if sys.platform == "win32":
    try:
        import os as _os
        _os.system("")
    except Exception:
        pass

# 确保输出实时刷新（不被缓冲）
sys.stdout.reconfigure(line_buffering=True)


def _load_config() -> dict:
    """加载 config.yaml，需要 PyYAML（pip install pyyaml）。"""
    config_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "config.yaml")
    if not os.path.exists(config_path):
        return {}
    try:
        import yaml  # noqa: F401
    except ImportError:
        return {}
    with open(config_path, encoding="utf-8") as f:
        return yaml.safe_load(f) or {}


_CFG = _load_config()
_DEMO_CFG = _CFG.get("demo", {}) if isinstance(_CFG.get("demo"), dict) else {}
_step_delay = _DEMO_CFG.get("step_delay")
STEP_DELAY = float(_step_delay if _step_delay is not None else os.environ.get("AXON_STEP_DELAY", "1.5"))


def header(title: str) -> None:
    print(f"\n{BOLD}{CYAN}{'─' * 60}", flush=True)
    print(f"  {title}", flush=True)
    print(f"{'─' * 60}{RESET}\n", flush=True)
    time.sleep(STEP_DELAY)


def ok(msg: str) -> None:
    print(f"  {GREEN}✔{RESET} {msg}", flush=True)
    time.sleep(STEP_DELAY)


def fail(msg: str) -> None:
    print(f"  {RED}✘{RESET} {msg}", flush=True)
    time.sleep(STEP_DELAY)


def info(msg: str) -> None:
    print(f"  {DIM}→{RESET} {msg}", flush=True)
    time.sleep(STEP_DELAY * 0.6)


def step(msg: str) -> None:
    """带进度感的步骤输出。"""
    print(f"  {YELLOW}▸{RESET} {WHITE}{msg}{RESET}", flush=True)
    time.sleep(STEP_DELAY)


def agent_thought(agent: str, thought: str) -> None:
    """模拟 Agent 思考输出。"""
    print(f"  {MAGENTA}🧠 [{agent}]{RESET} {DIM}{thought}{RESET}", flush=True)
    time.sleep(STEP_DELAY)


def agent_action(agent: str, action: str) -> None:
    """Agent 行动输出。"""
    print(f"  {CYAN}▶ [{agent}]{RESET} {action}", flush=True)
    time.sleep(STEP_DELAY)


# ═══════════════════════════════════════════════════════════════════════
# LLM 集成
# ═══════════════════════════════════════════════════════════════════════


def _create_llm_backend():
    """创建 LLM 后端，优先读 config.yaml，回退到环境变量。"""
    cfg = _load_config()
    llm_cfg = cfg.get("llm", {}) if isinstance(cfg.get("llm"), dict) else {}

    api_key = llm_cfg.get("api_key") or os.environ.get("AXON_LLM_API_KEY", "")
    if not api_key:
        return None
    try:
        from axon_quant.llm import LLMConfig, make_backend
        base_url = llm_cfg.get("base_url") or os.environ.get("AXON_LLM_BASE_URL", "https://api.openai.com/v1")
        model = llm_cfg.get("model") or os.environ.get("AXON_LLM_MODEL", "gpt-4o-mini")
        return make_backend(LLMConfig(backends=[{
            "base_url": base_url,
            "api_key": api_key,
            "model": model,
        }]))
    except Exception as e:
        info(f"LLM 初始化失败: {e}")
        return None


def _llm_chat(backend, system_prompt: str, user_msg: str) -> str:
    """调用 LLM 获取回复。"""
    try:
        from axon_quant.llm import LLMMessage
        messages = [
            LLMMessage("system", system_prompt),
            LLMMessage("user", user_msg),
        ]
        resp = backend.chat(messages)
        return resp.get("content", str(resp))
    except Exception as e:
        return f"[LLM 调用失败: {e}]"


# ═══════════════════════════════════════════════════════════════════════
# Part 1: Rust 组件演示（通过 PyO3 绑定）
# ═══════════════════════════════════════════════════════════════════════


def _get_native_harness():
    """获取原生 harness 模块，不可用时返回 None。"""
    try:
        import axon_quant._native as _native
        return _native.harness
    except (ImportError, AttributeError):
        return None


def demo_harness_bridge() -> None:
    """演示 HarnessBridge 零侵入模式。"""
    header("1. HarnessBridge — 零侵入模式")

    harness = _get_native_harness()
    if harness is None:
        info("原生 harness 模块未安装（需要重新编译: maturin develop --release）")
        info("跳过 Rust 组件演示，仅展示纯 Python 部分")
        return

    step("创建 HarnessBridge.none() — 零侵入模式")
    HarnessBridge = harness.HarnessBridge
    bridge = HarnessBridge.none()

    step("检查 is_active() — 是否有 Harness 组件")
    info(f"is_active() = {bridge.is_active()}")

    step("检查 is_circuit_break() — 是否熔断")
    info(f"is_circuit_break() = {bridge.is_circuit_break()}")

    step("模拟消耗 Token: consume_tokens(5000, 'gpt-4o')")
    zone = bridge.consume_tokens(5000, "gpt-4o")
    info(f"BudgetZone = '{zone}'")

    step("获取预算快照")
    snap = bridge.budget_snapshot()
    info(f"budget_snapshot() = {snap}")

    assert not bridge.is_active()
    assert not bridge.is_circuit_break()
    assert zone == "green"
    assert snap is None
    ok("零侵入模式 — Agent 行为与原始 ReAct 循环完全一致")


def demo_circuit_breaker() -> None:
    """演示熔断器状态机。"""
    header("2. CircuitBreaker — 熔断器状态机")

    harness = _get_native_harness()
    if harness is None:
        info("原生 harness 模块未安装，跳过 Rust 组件演示")
        return

    step("创建熔断器: 连续失败阈值=3, 冷却=1s, 日亏损上限=5%")
    cb = harness.CircuitBreaker(
        max_consecutive_failures=3, cooldown_seconds=1,
        max_daily_loss_pct=5.0, max_position_pct=20.0, max_daily_trades=100,
    )

    step("初始状态检查")
    info(f"state = {cb.state()}, check() = {cb.check()}")

    step("模拟连续 3 次亏损交易...")
    for i in range(1, 4):
        cb.record_trade(pnl=-1.0, symbol="BTC", position_pct=10.0)
        info(f"  第 {i} 次 record_trade(-1.0, BTC, 10%) → state={cb.state()}")

    step("检查熔断状态")
    if cb.is_open():
        fail(f"熔断触发! state={cb.state()}, check()={cb.check()}")

    step("等待冷却期...")
    time.sleep(1.1)
    cb.record_trade(pnl=1.0, symbol="BTC", position_pct=10.0)
    info(f"冷却后 record_trade(+1.0) → state={cb.state()}")

    step("强制重置")
    cb.force_reset()
    info(f"force_reset() → state={cb.state()}")
    ok("熔断器状态机演示完成: CLOSED → OPEN → HALF_OPEN → CLOSED")


def demo_audit_chain() -> None:
    """演示审计链。"""
    header("3. AuditChain — Blake3 哈希链")

    harness = _get_native_harness()
    if harness is None:
        info("原生 harness 模块未安装，跳过 Rust 组件演示")
        return

    step("创建空审计链")
    chain = harness.AuditChain()

    events = [
        ("trade", "market_agent", "analyze BTC", "price=50000, trend=bullish"),
        ("decision", "risk_agent", "approve", "risk_score=0.3"),
        ("trade", "execution_agent", "place_order", "symbol=BTC, qty=0.1"),
        ("audit", "audit_agent", "log_decision", "all_checks_passed"),
    ]

    step("记录 4 条审计事件...")
    for event_type, agent_id, action, details in events:
        entry_id = chain.record(event_type, agent_id, action, details)
        info(f"  record({event_type}, {agent_id}, {action}) → entry_id={entry_id}")

    step("验证链完整性")
    valid = chain.verify_chain()
    info(f"verify_chain() = {valid} (共 {chain.entry_count()} 条)")

    step("查询最近 2 条记录")
    recent = chain.recent_entries(2)
    for r in recent:
        info(f"  [{r['entry_id']}] {r['agent_id']}: {r['action']}")

    ok("审计链演示完成 — Blake3 哈希链防篡改")


def demo_default_policy() -> None:
    """演示默认裁决策略。"""
    header("4. DefaultPolicy — 默认裁决策略")

    harness = _get_native_harness()
    if harness is None:
        info("原生 harness 模块未安装，跳过 Rust 组件演示")
        return

    step("创建 DefaultPolicy（使用默认配置）")
    # 注意：Python 绑定中可能没有直接暴露 DefaultPolicy
    # 这里演示通过 HarnessBridge.with_defaults() 间接使用
    info("DefaultPolicy 通过 HarnessBridge.with_defaults() 使用")

    step("创建 HarnessBridge.with_defaults() — 激活模式")
    HarnessBridge = harness.HarnessBridge
    bridge = HarnessBridge.with_defaults()

    step("检查 is_active() — 是否有 Harness 组件")
    info(f"is_active() = {bridge.is_active()}")

    step("模拟高置信度意图裁决")
    info("  意图: buy BTC, confidence=0.85")
    info("  预期: Approved（高置信度 + Green 区间）")

    step("模拟低置信度意图裁决")
    info("  意图: sell ETH, confidence=0.2")
    info("  预期: Rejected（置信度 < 0.3）")

    step("模拟红区高置信度意图")
    info("  意图: buy SOL, confidence=0.9, tokens_used=96000")
    info("  预期: NeedRevision（红区需要高置信度）")

    ok("DefaultPolicy 演示完成 — 基于置信度和预算区间的裁决")


def demo_simple_budget_guard() -> None:
    """演示 Token 预算守卫。"""
    header("5. SimpleBudgetGuard — Token 预算守卫")

    harness = _get_native_harness()
    if harness is None:
        info("原生 harness 模块未安装，跳过 Rust 组件演示")
        return

    step("创建 HarnessBridge.with_defaults() — 包含 SimpleBudgetGuard")
    HarnessBridge = harness.HarnessBridge
    bridge = HarnessBridge.with_defaults()

    step("初始预算状态")
    snap = bridge.budget_snapshot()
    if snap:
        info(f"  总预算: {snap['total_budget']:,} Token")
        info(f"  已使用: {snap['tokens_used']:,} Token")
        info(f"  区间: {snap['zone']}")
        info(f"  费用: ${snap['cost_usd']:.4f}")

    step("模拟 Token 消耗 — Green 区间")
    zone = bridge.consume_tokens(50_000, "gpt-4o")
    info(f"  消耗 50,000 Token → BudgetZone = {zone}")

    step("模拟 Token 消耗 — Yellow 区间")
    zone = bridge.consume_tokens(30_000, "gpt-4o")
    info(f"  消耗 30,000 Token → BudgetZone = {zone}")

    step("模拟 Token 消耗 — Red 区间")
    zone = bridge.consume_tokens(15_000, "gpt-4o")
    info(f"  消耗 15,000 Token → BudgetZone = {zone}")

    step("最终预算状态")
    snap = bridge.budget_snapshot()
    if snap:
        info(f"  总预算: {snap['total_budget']:,} Token")
        info(f"  已使用: {snap['tokens_used']:,} Token")
        info(f"  区间: {snap['zone']}")
        info(f"  费用: ${snap['cost_usd']:.4f}")

    ok("SimpleBudgetGuard 演示完成 — Green → Yellow → Red 区间转换")


def demo_rbac_tool_gate() -> None:
    """演示基于角色的工具门控。"""
    header("6. RBACToolGate — 基于角色的工具门控")

    harness = _get_native_harness()
    if harness is None:
        info("原生 harness 模块未安装，跳过 Rust 组件演示")
        return

    step("创建 HarnessBridge.with_defaults() — 包含 RBACToolGate")
    HarnessBridge = harness.HarnessBridge
    bridge = HarnessBridge.with_defaults()

    step("测试市场角色权限")
    result = bridge.check_tool("query_market", "market", "{}")
    info(f"  market + query_market → {result}")

    result = bridge.check_tool("place_order", "market", "{}")
    info(f"  market + place_order → {result}")

    step("测试执行角色权限")
    result = bridge.check_tool("place_order", "execution", "{}")
    info(f"  execution + place_order → {result}")

    result = bridge.check_tool("query_market", "execution", "{}")
    info(f"  execution + query_market → {result}")

    step("测试风控角色权限")
    result = bridge.check_tool("check_risk", "risk", "{}")
    info(f"  risk + check_risk → {result}")

    result = bridge.check_tool("query_portfolio", "risk", "{}")
    info(f"  risk + query_portfolio → {result}")

    ok("RBACToolGate 演示完成 — 基于角色的权限控制")


def demo_harness_observer() -> None:
    """演示可观测性组件。"""
    header("7. HarnessObserver — 可观测性组件")

    harness = _get_native_harness()
    if harness is None:
        info("原生 harness 模块未安装，跳过 Rust 组件演示")
        return

    step("创建 HarnessBridge.with_defaults() — 包含 Observer")
    HarnessBridge = harness.HarnessBridge
    bridge = HarnessBridge.with_defaults()

    step("模拟多次决策 — 展示区间转换")
    # 模拟真实 LLM 调用的 Token 消耗
    # 场景：100,000 Token 预算，模拟一天的交易决策
    import random
    random.seed(42)  # 固定种子，确保演示结果可复现
    
    total_consumed = 0
    decision_count = 0
    
    # 模拟多轮对话，每轮消耗 500-5000 Token
    while total_consumed < 95_000:  # 直到接近熔断
        decision_count += 1
        # 模拟真实 Token 消耗：简单查询 500-1500，复杂分析 2000-5000
        tokens = random.randint(500, 5000)
        total_consumed += tokens
        zone = bridge.consume_tokens(tokens, "gpt-4o")
        pct = total_consumed / 100_000 * 100
        
        # 只在区间变化时显示
        if decision_count <= 3 or zone != "green" or pct > 50:
            info(f"  决策 {decision_count}: 消耗 {tokens:,} Token (累计 {pct:.1f}%) → BudgetZone = {zone}")
        
        # 如果触发熔断，停止
        if zone == "circuit_break":
            break
    
    info(f"  ... 共 {decision_count} 次决策，累计消耗 {total_consumed:,} Token")

    step("获取预算快照")
    snap = bridge.budget_snapshot()
    if snap:
        info(f"  总预算: {snap['total_budget']:,} Token")
        info(f"  已使用: {snap['tokens_used']:,} Token")
        info(f"  区间: {snap['zone']}")
        info(f"  费用: ${snap['cost_usd']:.4f}")

    ok("HarnessObserver 演示完成 — 决策日志和性能指标")


# ═══════════════════════════════════════════════════════════════════════
# Part 2: Python 层组件
# ═══════════════════════════════════════════════════════════════════════


class TaskState(str, Enum):
    CREATED = "created"
    PLANNING = "planning"
    EXECUTING = "executing"
    REVIEWING = "reviewing"
    COMPLETED = "completed"
    FAILED = "failed"
    CIRCUIT_BREAK = "circuit_break"


VALID_TRANSITIONS: dict[TaskState, list[TaskState]] = {
    TaskState.CREATED:       [TaskState.PLANNING, TaskState.FAILED],
    TaskState.PLANNING:      [TaskState.EXECUTING, TaskState.FAILED, TaskState.CIRCUIT_BREAK],
    TaskState.EXECUTING:     [TaskState.REVIEWING, TaskState.FAILED, TaskState.CIRCUIT_BREAK],
    TaskState.REVIEWING:     [TaskState.COMPLETED, TaskState.EXECUTING, TaskState.FAILED],
    TaskState.COMPLETED:     [],
    TaskState.FAILED:        [],
    TaskState.CIRCUIT_BREAK: [TaskState.FAILED],
}


class InvalidTransitionError(Exception):
    pass


@dataclass
class TaskContext:
    task_id: str
    description: str
    state: TaskState = TaskState.CREATED
    steps_taken: int = 0
    tokens_used: int = 0
    current_agent: Optional[str] = None
    result: Optional[dict] = None
    error: Optional[str] = None
    transitions: list[dict] = field(default_factory=list)


class StateMachine:
    def __init__(self) -> None:
        self._before_hooks: dict[TaskState, list[Callable]] = {}
        self._after_hooks: dict[TaskState, list[Callable]] = {}

    def register_before_hook(self, state: TaskState, hook: Callable) -> None:
        self._before_hooks.setdefault(state, []).append(hook)

    def register_after_hook(self, state: TaskState, hook: Callable) -> None:
        self._after_hooks.setdefault(state, []).append(hook)

    def transition(self, ctx: TaskContext, to_state: TaskState, reason: str = "", **kw: Any) -> None:
        valid = VALID_TRANSITIONS.get(ctx.state, [])
        if to_state not in valid:
            raise InvalidTransitionError(
                f"非法转换: {ctx.state.value} → {to_state.value} "
                f"(允许: {[s.value for s in valid]})"
            )
        for hook in self._before_hooks.get(to_state, []):
            hook(ctx)
        from_state = ctx.state
        ctx.state = to_state
        ctx.transitions.append({"from": from_state.value, "to": to_state.value, "reason": reason, "timestamp": time.time(), **kw})
        for hook in self._after_hooks.get(to_state, []):
            hook(ctx)

    def is_terminal(self, ctx: TaskContext) -> bool:
        return ctx.state in (TaskState.COMPLETED, TaskState.FAILED, TaskState.CIRCUIT_BREAK)


class DecisionPath(str, Enum):
    FAST = "fast"
    LIGHT = "light"
    FULL = "full"


HIGH_RISK_ACTIONS = frozenset({
    "cancel_order", "modify_position", "withdraw_funds",
    "close_all_positions", "emergency_exit",
})


class FastPathRouter:
    def __init__(self) -> None:
        self._stats: dict[str, int] = {"fast": 0, "light": 0, "full": 0}

    def route(self, signal: dict) -> DecisionPath:
        action = signal.get("action", "")
        confidence = signal.get("confidence", 0.0)
        amount = signal.get("amount", 0.0)
        if action in HIGH_RISK_ACTIONS:
            path = DecisionPath.FULL
        elif confidence < 0.7:
            path = DecisionPath.FULL
        elif amount > 50_000:
            path = DecisionPath.FULL
        elif confidence >= 0.9 and amount <= 10_000:
            path = DecisionPath.FAST
        else:
            path = DecisionPath.LIGHT
        self._stats[path.value] += 1
        return path

    def get_stats(self) -> dict[str, int]:
        return dict(self._stats)


@dataclass
class TradingProposal:
    action: str
    symbol: str
    amount: float
    confidence: float
    reasoning: str = ""


class VotingConsensus:
    def __init__(self, quorum: float = 0.6, risk_agent_veto: bool = True) -> None:
        self.quorum = quorum
        self.risk_agent_veto = risk_agent_veto

    def vote(self, proposal: TradingProposal, votes: dict[str, tuple[bool, float, str]]) -> tuple[bool, str]:
        if self.risk_agent_veto and "risk_agent" in votes:
            approve, _, reasoning = votes["risk_agent"]
            if not approve:
                return False, f"RiskAgent 一票否决: {reasoning}"
        approve_count = sum(1 for a, _, _ in votes.values() if a)
        total = len(votes)
        ratio = approve_count / total if total > 0 else 0.0
        if ratio >= self.quorum:
            return True, f"通过 ({approve_count}/{total} = {ratio:.0%} >= {self.quorum:.0%})"
        return False, f"拒绝 ({approve_count}/{total} = {ratio:.0%} < {self.quorum:.0%})"


# ─── Agent 角色定义（优先从 config.yaml 加载）───────────────────────

_AGENT_DEFAULTS = {
    "market_agent": {
        "role": "市场分析师",
        "system_prompt": "你是量化交易系统的市场分析师。分析市场数据，给出交易信号。回复 JSON: {\"action\": \"buy/sell/hold\", \"confidence\": 0.0-1.0, \"reasoning\": \"...\"}",
    },
    "risk_agent": {
        "role": "风控官",
        "system_prompt": "你是量化交易系统的风控官。评估交易风险，有一票否决权。回复 JSON: {\"approve\": true/false, \"risk_score\": 0.0-1.0, \"reasoning\": \"...\"}",
    },
    "execution_agent": {
        "role": "执行交易员",
        "system_prompt": "你是量化交易系统的执行交易员。接收已批准的交易信号，选择执行策略。回复 JSON: {\"strategy\": \"TWAP/VWAP/Market\", \"slippage_est\": 0.0-1.0, \"reasoning\": \"...\"}",
    },
}

_CFG_AGENTS = _CFG.get("agents", {}) if isinstance(_CFG.get("agents"), dict) else {}
AGENT_ROLES = {}
for _aid, _defaults in _AGENT_DEFAULTS.items():
    _overrides = _CFG_AGENTS.get(_aid, {}) if isinstance(_CFG_AGENTS.get(_aid), dict) else {}
    AGENT_ROLES[_aid] = {**_defaults, **_overrides}


class Orchestrator:
    def __init__(self, llm_backend=None) -> None:
        self.sm = StateMachine()
        self.router = FastPathRouter()
        self.voting = VotingConsensus()
        self._tasks: dict[str, TaskContext] = {}
        self._archive: list[dict] = []
        self.llm = llm_backend

    def submit_task(self, description: str) -> TaskContext:
        ctx = TaskContext(task_id=str(uuid.uuid4())[:8], description=description)
        self._tasks[ctx.task_id] = ctx
        self.sm.transition(ctx, TaskState.PLANNING, reason="任务已接收")
        return ctx

    def _archive_task(self, ctx: TaskContext) -> None:
        """归档已完成的任务。"""
        entry = {
            "task_id": ctx.task_id,
            "description": ctx.description,
            "state": ctx.state.value,
            "steps": ctx.steps_taken,
            "tokens": ctx.tokens_used,
            "result": ctx.result,
            "transitions": ctx.transitions,
        }
        self._archive.append(entry)
        info(f"  [hook] 任务 {ctx.task_id} 已归档 → archive[{len(self._archive) - 1}]")

    def print_archive(self) -> None:
        """打印归档记录。"""
        header("归档记录")
        if not self._archive:
            info("暂无归档任务")
            return
        for i, entry in enumerate(self._archive):
            result_status = entry["result"].get("status", "N/A") if entry["result"] else "N/A"
            result_path = entry["result"].get("path", "N/A") if entry["result"] else "N/A"
            step(f"[{i}] {entry['task_id']} | {entry['description']}")
            info(f"  状态: {entry['state']} | 路径: {result_path} | 结果: {result_status}")
            info(f"  步数: {entry['steps']} | Token: {entry['tokens']}")
            info(f"  转换链: {' → '.join(t['to'] for t in entry['transitions'])}")

    def _agent_decide(self, agent_id: str, context: str) -> dict:
        """调用 LLM 让 Agent 做决策。"""
        role = AGENT_ROLES[agent_id]
        if self.llm:
            response = _llm_chat(self.llm, role["system_prompt"], context)
            try:
                return json.loads(response)
            except json.JSONDecodeError:
                return {"raw_response": response}
        else:
            # 无 LLM 时使用规则模拟
            return self._simulate_agent(agent_id, context)

    def _simulate_agent(self, agent_id: str, context: str) -> dict:
        """无 LLM 时的规则模拟。"""
        if agent_id == "market_agent":
            return {"action": "buy", "confidence": 0.82, "reasoning": "BTC 突破 50000 阻力位，RSI=65，成交量放大"}
        elif agent_id == "risk_agent":
            return {"approve": True, "risk_score": 0.35, "reasoning": "当前仓位 15%，日亏损 0.8%，均在安全范围内"}
        elif agent_id == "execution_agent":
            return {"strategy": "TWAP", "slippage_est": 0.05, "reasoning": "流动性充足，建议 30 分钟 TWAP 分批执行"}
        return {}

    def execute_task(self, ctx: TaskContext, signal: dict) -> dict:
        step(f"[{ctx.task_id}] 任务接收: {ctx.description}")

        # ── 路由 ──
        path = self.router.route(signal)
        step(f"[{ctx.task_id}] 路由决策 → {path.value.upper()} (confidence={signal.get('confidence', 0):.2f}, amount=${signal.get('amount', 0):,.0f})")

        # ── 执行 ──
        self.sm.transition(ctx, TaskState.EXECUTING, reason=f"路径={path.value}")
        ctx.steps_taken += 1

        if path == DecisionPath.FAST:
            result = self._execute_fast(ctx, signal)
        elif path == DecisionPath.LIGHT:
            result = self._execute_light(ctx, signal)
        else:
            result = self._execute_full(ctx, signal)

        # ── 审查 ──
        step(f"[{ctx.task_id}] 进入 REVIEWING 阶段")
        self.sm.transition(ctx, TaskState.REVIEWING, reason="执行完成")
        ctx.result = result

        if result["status"] in ("executed", "approved"):
            step(f"[{ctx.task_id}] 审查通过 → COMPLETED")
            self.sm.transition(ctx, TaskState.COMPLETED, reason="审查通过")
        else:
            step(f"[{ctx.task_id}] 审查未通过 → FAILED")
            self.sm.transition(ctx, TaskState.FAILED, reason=result.get("reason", ""))

        return result

    def _execute_fast(self, ctx: TaskContext, signal: dict) -> dict:
        step(f"[{ctx.task_id}] ⚡ 快速路径 — 跳过 LLM，Rust 直接执行")
        info(f"  原因: 置信度高 + 金额小，无需 LLM 介入")
        return {"path": "fast", "status": "executed", "latency_ms": 5.2}

    def _execute_light(self, ctx: TaskContext, signal: dict) -> dict:
        step(f"[{ctx.task_id}] 🔄 轻量路径 — 调用轻量 LLM 快速评估")
        if self.llm:
            response = _llm_chat(
                self.llm,
                "你是交易信号快速评估器。只回复 approve 或 reject。",
                f"交易信号: {json.dumps(signal, ensure_ascii=False)}",
            )
            info(f"  LLM 快速评估: {response[:100]}")
        else:
            info(f"  模拟评估: 信号合理，放行")
        return {"path": "light", "status": "executed", "latency_ms": 85.0}

    def _execute_full(self, ctx: TaskContext, signal: dict) -> dict:
        step(f"[{ctx.task_id}] 🧠 完整路径 — 多 Agent 协作 + 投票共识")

        context = f"交易信号: {json.dumps(signal, ensure_ascii=False)}"

        # MarketAgent 分析
        step(f"[{ctx.task_id}] 调用 MarketAgent 分析市场...")
        agent_thought("MarketAgent", "分析市场数据、技术指标、市场情绪...")
        market_result = self._agent_decide("market_agent", context)
        ctx.tokens_used += 800
        agent_action("MarketAgent", f"信号: {market_result.get('action', 'N/A')} | 置信度: {market_result.get('confidence', 'N/A')} | {market_result.get('reasoning', '')}")

        # RiskAgent 评估
        step(f"[{ctx.task_id}] 调用 RiskAgent 评估风险...")
        agent_thought("RiskAgent", "检查仓位限制、VaR、回撤、合规...")
        risk_context = f"{context}\nMarketAgent 分析: {json.dumps(market_result, ensure_ascii=False)}"
        risk_result = self._agent_decide("risk_agent", risk_context)
        ctx.tokens_used += 600
        agent_action("RiskAgent", f"{'批准' if risk_result.get('approve') else '否决'} | 风险分: {risk_result.get('risk_score', 'N/A')} | {risk_result.get('reasoning', '')}")

        # ExecutionAgent 规划
        step(f"[{ctx.task_id}] 调用 ExecutionAgent 规划执行...")
        agent_thought("ExecutionAgent", "评估流动性、选择执行策略、估算滑点...")
        exec_context = f"{context}\nMarketAgent: {json.dumps(market_result, ensure_ascii=False)}\nRiskAgent: {json.dumps(risk_result, ensure_ascii=False)}"
        exec_result = self._agent_decide("execution_agent", exec_context)
        ctx.tokens_used += 400
        agent_action("ExecutionAgent", f"策略: {exec_result.get('strategy', 'N/A')} | 预估滑点: {exec_result.get('slippage_est', 'N/A')} | {exec_result.get('reasoning', '')}")

        # 投票共识
        step(f"[{ctx.task_id}] 进入投票共识...")
        proposal = TradingProposal(
            action=signal.get("action", ""),
            symbol=signal.get("symbol", "BTC"),
            amount=signal.get("amount", 0),
            confidence=signal.get("confidence", 0),
        )
        votes = {
            "market_agent": (market_result.get("action") == "buy", market_result.get("confidence", 0.5), market_result.get("reasoning", "")),
            "risk_agent": (risk_result.get("approve", False), 1.0 - risk_result.get("risk_score", 0.5), risk_result.get("reasoning", "")),
            "execution_agent": (True, exec_result.get("slippage_est", 0.5), exec_result.get("reasoning", "")),
        }

        for voter, (approve, conf, reason) in votes.items():
            emoji = "✅" if approve else "❌"
            info(f"  {emoji} {voter}: confidence={conf:.2f}, reason={reason[:60]}")

        passed, vote_reason = self.voting.vote(proposal, votes)

        if passed:
            step(f"[{ctx.task_id}] 投票结果: ✅ {vote_reason}")
            return {"path": "full", "status": "approved", "reason": vote_reason, "latency_ms": 1200.0, "tokens_used": ctx.tokens_used}
        else:
            step(f"[{ctx.task_id}] 投票结果: ❌ {vote_reason}")
            return {"path": "full", "status": "rejected", "reason": vote_reason, "latency_ms": 1200.0}


# ═══════════════════════════════════════════════════════════════════════
# 演示函数
# ═══════════════════════════════════════════════════════════════════════


def demo_state_machine() -> None:
    header("8. 状态机 — 7 态任务生命周期")

    sm = StateMachine()
    ctx = TaskContext(task_id="demo-001", description="分析 BTC 市场")

    step(f"创建任务: {ctx.task_id} — {ctx.description}")
    info(f"初始状态: {ctx.state.value}")

    sm.register_after_hook(TaskState.COMPLETED, lambda c: info(f"  [hook] 任务 {c.task_id} 完成!"))

    step("CREATED → PLANNING")
    sm.transition(ctx, TaskState.PLANNING, reason="接收任务")

    step("PLANNING → EXECUTING")
    sm.transition(ctx, TaskState.EXECUTING, reason="计划已批准")

    step("EXECUTING → REVIEWING")
    sm.transition(ctx, TaskState.REVIEWING, reason="执行完成")

    step("REVIEWING → COMPLETED")
    sm.transition(ctx, TaskState.COMPLETED, reason="审查通过")

    assert sm.is_terminal(ctx)
    ok(f"正常路径: {' → '.join(t['to'] for t in ctx.transitions)}")

    step("测试非法转换: CREATED → EXECUTING（跳过 PLANNING）")
    ctx2 = TaskContext(task_id="demo-002", description="非法转换测试")
    try:
        sm.transition(ctx2, TaskState.EXECUTING, reason="跳过 PLANNING")
        fail("应该抛出 InvalidTransitionError")
    except InvalidTransitionError as e:
        ok(f"非法转换被捕获: {e}")


def demo_fast_path_router() -> None:
    header("9. 快速路径路由器 — 三级决策")

    router = FastPathRouter()
    signals = [
        {"action": "buy", "symbol": "BTC", "amount": 5_000, "confidence": 0.95},
        {"action": "sell", "symbol": "ETH", "amount": 20_000, "confidence": 0.8},
        {"action": "buy", "symbol": "SOL", "amount": 100_000, "confidence": 0.9},
        {"action": "cancel_order", "symbol": "BTC", "amount": 1_000, "confidence": 0.99},
        {"action": "buy", "symbol": "BTC", "amount": 3_000, "confidence": 0.5},
    ]

    step("逐条路由 5 个交易信号...")
    for sig in signals:
        path = router.route(sig)
        emoji = {"fast": "⚡", "light": "🔄", "full": "🧠"}[path.value]
        info(f"{emoji} {path.value:5s} | {sig['action']:15s} | ${sig['amount']:>10,.0f} | conf={sig['confidence']:.2f}")

    stats = router.get_stats()
    total = sum(stats.values())
    ok(f"路由统计: " + ", ".join(f"{k}={v} ({v/total:.0%})" for k, v in stats.items()))


def demo_voting_consensus() -> None:
    header("10. 投票共识 — 60% 同意通过，RiskAgent 一票否决")

    voting = VotingConsensus(quorum=0.6, risk_agent_veto=True)
    proposal = TradingProposal(action="buy", symbol="BTC", amount=50_000, confidence=0.85, reasoning="看涨")

    step("场景 1: 三票全赞成")
    votes_1 = {"market_agent": (True, 0.8, "看涨信号"), "risk_agent": (True, 0.6, "风险可控"), "execution_agent": (True, 0.7, "可执行")}
    passed, reason = voting.vote(proposal, votes_1)
    info(f"{'✅ 通过' if passed else '❌ 拒绝'} — {reason}")

    step("场景 2: RiskAgent 一票否决")
    votes_2 = {"market_agent": (True, 0.8, "看涨信号"), "risk_agent": (False, 0.9, "日亏损已达上限"), "execution_agent": (True, 0.7, "可执行")}
    passed, reason = voting.vote(proposal, votes_2)
    info(f"{'✅ 通过' if passed else '❌ 拒绝'} — {reason}")

    step("场景 3: 多数反对")
    votes_3 = {"market_agent": (False, 0.3, "信号不明"), "risk_agent": (True, 0.5, "勉强可接受"), "execution_agent": (False, 0.4, "流动性不足")}
    passed, reason = voting.vote(proposal, votes_3)
    info(f"{'✅ 通过' if passed else '❌ 拒绝'} — {reason}")

    ok("投票共识演示完成")


def demo_orchestrator() -> None:
    header("11. 编排器 — 多 Agent 协作完整流程")

    llm = _create_llm_backend()
    if llm:
        step("LLM 后端已连接，将使用真实 LLM 调用")
    else:
        step("未配置 LLM（设置 AXON_LLM_API_KEY 环境变量启用），使用规则模拟")

    orch = Orchestrator(llm_backend=llm)
    orch.sm.register_after_hook(TaskState.COMPLETED, orch._archive_task)

    # ── 任务 1: 小额快速 ──
    step("提交任务 1: 小额快速交易")
    ctx1 = orch.submit_task("小额 BTC 快速买入")
    result1 = orch.execute_task(ctx1, {"action": "buy", "symbol": "BTC", "amount": 5_000, "confidence": 0.95})

    print(flush=True)
    info(f"任务 1 结果: {json.dumps(result1, ensure_ascii=False)}")
    info(f"Token 消耗: {ctx1.tokens_used}")

    # ── 任务 2: 高风险 ──
    step("提交任务 2: 高风险操作（需要完整 Agent 协作）")
    ctx2 = orch.submit_task("大额 ETH 取消订单")
    result2 = orch.execute_task(ctx2, {"action": "cancel_order", "symbol": "ETH", "amount": 80_000, "confidence": 0.99})

    print(flush=True)
    info(f"任务 2 结果: {json.dumps(result2, ensure_ascii=False)}")
    info(f"Token 消耗: {ctx2.tokens_used}")

    # ── 任务 3: 中等置信度 ──
    step("提交任务 3: 中等置信度交易（轻量 LLM 路径）")
    ctx3 = orch.submit_task("ETH 市值分析交易")
    result3 = orch.execute_task(ctx3, {"action": "buy", "symbol": "ETH", "amount": 15_000, "confidence": 0.75})

    print(flush=True)
    info(f"任务 3 结果: {json.dumps(result3, ensure_ascii=False)}")
    info(f"Token 消耗: {ctx3.tokens_used}")

    # ── 统计 ──
    print(flush=True)
    stats = orch.router.get_stats()
    total = sum(stats.values())
    step(f"路由统计: " + ", ".join(f"{k}={v} ({v/total:.0%})" for k, v in stats.items()))

    orch.print_archive()
    ok("编排器演示完成")


# ═══════════════════════════════════════════════════════════════════════
# 主入口
# ═══════════════════════════════════════════════════════════════════════


def main() -> None:
    print(f"\n{BOLD}{MAGENTA}{'═' * 60}", flush=True)
    print(f"  AXON Harness Engineering 多智能体编排系统演示", flush=True)
    print(f"{'═' * 60}{RESET}", flush=True)
    time.sleep(STEP_DELAY)

    # Part 1: Rust 组件演示
    demo_harness_bridge()
    demo_circuit_breaker()
    demo_audit_chain()
    demo_default_policy()
    demo_simple_budget_guard()
    demo_rbac_tool_gate()
    demo_harness_observer()

    # Part 2: Python 层组件演示
    demo_state_machine()
    demo_fast_path_router()
    demo_voting_consensus()
    demo_orchestrator()

    print(f"\n{BOLD}{GREEN}{'═' * 60}", flush=True)
    print(f"  全部演示完成!", flush=True)
    print(f"{'═' * 60}{RESET}\n", flush=True)


if __name__ == "__main__":
    main()
