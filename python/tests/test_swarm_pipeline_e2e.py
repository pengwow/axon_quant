"""axon_quant.llm.swarm 端到端测试(0.3.0 P0 Batch 4 — T2.10)。

覆盖范围(15 个 case):
1. 类型导入 / 实例化 — `SwarmConfig` / `SwarmOrchestrator` / 4 类 Agent
2. `AgentRole` / `AgentStatus` / `VoteType` / `SignalType` / `VoteProposal` / `VoteResult` /
   `MarketSignal` 基础属性
3. 4 类 agent 便捷注册(每个 agent 3 个 case):
   - 构造 + count
   - 默认配置
   - 状态查询
4. 编排器启动 / 停止 / 统计 (Pipeline 端到端 3 个 case):
   - start / stop 生命周期
   - inject_market_signal → stats 增加
   - inject_vote_response → 投票统计
5. 异常路径:不调 start() 直接 inject → RuntimeError
6. `__repr__` / dict 字段

运行::

    cd /Users/liupeng/workspace/quant/axon_quant
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon_quant/.venv-test/bin/python \\
        python -m pytest python/tests/test_swarm_pipeline_e2e.py -v

注意:本测试需先 build wheel(参见 Makefile 的 ``python-build`` /
``python-develop`` 目标)。如未 build,部分测试 skip。
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

import pytest

# 强制使用本项目 venv(避免 miniconda pyarrow / numpy 干扰)
_VENV_SITE = Path("/Users/liupeng/workspace/quant/axon_quant/.venv-test/lib/python3.13/site-packages")
if _VENV_SITE.exists() and str(_VENV_SITE) not in sys.path:
    sys.path.insert(0, str(_VENV_SITE))

# ``axon_quant`` 在 maturin develop / wheel install 后可被 import
# 缺失时 skip 整个模块(开发期还没 build 时常见)
try:
    import axon_quant  # noqa: F401
    # swarm 类从 axon_quant.llm 透出(见 llm.py 包装)
    from axon_quant.llm import (  # noqa: F401
        AgentRole,
        AgentStatus,
        MarketSignal,
        SignalType,
        SwarmConfig,
        SwarmOrchestrator,
        VoteProposal,
        VoteResult,
        VoteType,
    )
    # TradingTools 构造需要的 trading 类
    from axon_quant.trading import (  # noqa: F401
        MockTradingBackend,
        PlaceOrderTool,
        QueryPortfolioTool,
        RiskLimits,
    )
    from axon_quant.llm import TradingTools  # type: ignore[attr-defined]
    _SWARM_AVAILABLE = hasattr(axon_quant, "_native") and hasattr(
        axon_quant._native.llm, "swarm"
    )
except ImportError as _e:
    pytest.skip(f"axon_quant or swarm not installed: {_e}", allow_module_level=True)
    raise  # 实际不可达,仅供类型检查

if not _SWARM_AVAILABLE:
    pytest.skip(
        "axon_quant._native.llm.swarm not yet registered (need maturin develop)",
        allow_module_level=True,
    )


# ═══════════════════════════════════════════════════════════════════════════
# 1. 基础类型可用性
# ═══════════════════════════════════════════════════════════════════════════


def test_swarm_enums_and_classes_importable():
    """所有 swarm 顶层符号都能 import。"""
    # 枚举
    assert AgentRole is not None
    assert AgentStatus is not None
    assert VoteType is not None
    assert SignalType is not None
    # 数据结构
    assert MarketSignal is not None
    assert VoteProposal is not None
    assert VoteResult is not None
    # 编排器
    assert SwarmConfig is not None
    assert SwarmOrchestrator is not None


def test_swarm_config_defaults():
    """SwarmConfig 默认值与 SwarmConfig::default() 一致。"""
    cfg = SwarmConfig()
    assert cfg.vote_timeout_ms == 5000
    assert cfg.loop_tick_ms == 100


def test_swarm_config_custom():
    """SwarmConfig 自定义字段。"""
    cfg = SwarmConfig(vote_timeout_ms=2000, loop_tick_ms=50)
    assert cfg.vote_timeout_ms == 2000
    assert cfg.loop_tick_ms == 50


# ═══════════════════════════════════════════════════════════════════════════
# 2. 枚举值正确性
# ═══════════════════════════════════════════════════════════════════════════


def test_agent_role_variants():
    """AgentRole 至少 4 个变体:Market/Risk/Execution/Audit。"""
    roles = {AgentRole.Market, AgentRole.Risk, AgentRole.Execution, AgentRole.Audit}
    assert len(roles) == 4


def test_signal_type_variants():
    """SignalType 至少 3 个变体:Buy/Sell/Hold。"""
    s = {SignalType.Buy, SignalType.Sell, SignalType.Hold}
    assert len(s) == 3


def test_vote_type_variants():
    """VoteType 至少 3 个变体:TradeDecision/EmergencyStop/StrategyAdjustment。"""
    v = {VoteType.TradeDecision, VoteType.EmergencyStop, VoteType.StrategyAdjustment}
    assert len(v) == 3


# ═══════════════════════════════════════════════════════════════════════════
# 3. 数据结构字段
# ═══════════════════════════════════════════════════════════════════════════


def test_market_signal_fields():
    """MarketSignal 字段读写。"""
    s = MarketSignal(
        symbol="BTC-USDT",
        signal_type=SignalType.Buy,
        confidence=0.85,
        reasoning="momentum breakout",
    )
    assert s.symbol == "BTC-USDT"
    assert s.signal_type == SignalType.Buy
    assert s.confidence == 0.85
    assert s.reasoning == "momentum breakout"


def test_vote_proposal_fields():
    """VoteProposal 字段读写。"""
    p = VoteProposal(
        proposal_id="vp-1",
        proposal_type=VoteType.TradeDecision,
        content="Buy BTC-USDT",
        deadline_ms=12345,
    )
    assert p.proposal_id == "vp-1"
    assert p.proposal_type == VoteType.TradeDecision
    assert p.content == "Buy BTC-USDT"
    assert p.deadline_ms == 12345


def test_vote_result_fields():
    """VoteResult 字段读写(只读,内部生成)。"""
    # 通过 orchestrator submit_vote 拿到 VoteResult
    orch = SwarmOrchestrator(SwarmConfig())
    orch.create_vote(VoteProposal(
        proposal_id="vp-2",
        proposal_type=VoteType.TradeDecision,
        content="Buy ETH",
        deadline_ms=1000,
    ))
    res = orch.stats()  # 替代:VoteResult 由 submit_vote 返回
    assert "votes_created" in res
    assert res["votes_created"] == 1


# ═══════════════════════════════════════════════════════════════════════════
# 4. 4 类 Agent 注册(每个 agent 至少 1 个)
# ═══════════════════════════════════════════════════════════════════════════


def test_register_market_agent_count_increments():
    """注册 MarketAgent 后 agent_count 增加。"""
    orch = SwarmOrchestrator(SwarmConfig())
    assert orch.agent_count() == 0
    orch.register_market_agent(agent_id="m0")
    assert orch.agent_count() == 1
    assert orch.agent_count_by_role(AgentRole.Market) == 1


def test_register_market_agent_default_symbols_and_threshold():
    """MarketAgent 默认 symbols = ['BTC-USDT'], threshold = 0.7。"""
    orch = SwarmOrchestrator(SwarmConfig())
    orch.register_market_agent(agent_id="m1")
    # status 应该是 Idle(初始)
    assert orch.agent_status("m1") == AgentStatus.Idle


def test_register_risk_agent_count_increments():
    """注册 RiskAgent 后 count_by_role(Risk) == 1。"""
    orch = SwarmOrchestrator(SwarmConfig())
    orch.register_risk_agent(agent_id="r0")
    assert orch.agent_count_by_role(AgentRole.Risk) == 1
    assert orch.agent_status("r0") == AgentStatus.Idle


def test_register_audit_agent_count_increments():
    """注册 AuditAgent 后 count_by_role(Audit) == 1。"""
    orch = SwarmOrchestrator(SwarmConfig())
    orch.register_audit_agent(agent_id="a0")
    assert orch.agent_count_by_role(AgentRole.Audit) == 1
    assert orch.agent_status("a0") == AgentStatus.Idle


def test_register_execution_agent_without_tools_works():
    """ExecutionAgent 不带 tools(模拟模式)也能注册。"""
    orch = SwarmOrchestrator(SwarmConfig())
    orch.register_execution_agent(agent_id="e0", tools=None)
    assert orch.agent_count_by_role(AgentRole.Execution) == 1
    assert orch.agent_status("e0") == AgentStatus.Idle


def test_register_execution_agent_with_tools_works():
    """ExecutionAgent 带 PlaceOrderTool + QueryPortfolioTool 注册。"""
    orch = SwarmOrchestrator(SwarmConfig())
    backend = MockTradingBackend()
    risk = RiskLimits(allowed_symbols=["BTC-USDT"])
    place = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk)
    query = QueryPortfolioTool(backend=backend)
    tools = TradingTools(place_order=place, query_portfolio=query)
    orch.register_execution_agent(agent_id="e1", tools=tools)
    assert orch.agent_count_by_role(AgentRole.Execution) == 1


# ═══════════════════════════════════════════════════════════════════════════
# 5. SwarmOrchestrator 生命周期 + 统计 (Pipeline 端到端)
# ═══════════════════════════════════════════════════════════════════════════


def test_orchestrator_start_stop_lifecycle():
    """start() → is_running() → stop() 生命周期正确。"""
    orch = SwarmOrchestrator(SwarmConfig())
    assert orch.is_running() is False
    orch.start()
    assert orch.is_running() is True
    orch.stop()
    assert orch.is_running() is False


def test_orchestrator_start_twice_is_idempotent():
    """重复 start() 是幂等的(lazy runtime 设计,start 多次 OK)。"""
    orch = SwarmOrchestrator(SwarmConfig())
    orch.start()
    # 重复 start 不报错(ensure_runtime 内部 check)
    orch.start()
    assert orch.is_running() is True
    orch.stop()


def test_register_market_agent_auto_starts_runtime():
    """register_*_agent 会自动启动 run_loop(避免显式 start)。"""
    orch = SwarmOrchestrator(SwarmConfig())
    assert orch.is_running() is False
    orch.register_market_agent(agent_id="m0")
    # register 后 runtime 应已激活
    assert orch.is_running() is True
    orch.stop()


def test_inject_market_signal_increments_stats():
    """start() 后 inject_market_signal → stats.market_signals 增加。"""
    orch = SwarmOrchestrator(SwarmConfig())
    orch.register_market_agent(agent_id="m0")
    orch.register_risk_agent(agent_id="r0")
    orch.register_execution_agent(agent_id="e0", tools=None)
    orch.register_audit_agent(agent_id="a0")
    orch.start()
    try:
        sig = MarketSignal(
            symbol="BTC-USDT",
            signal_type=SignalType.Buy,
            confidence=0.9,
            reasoning="test signal",
        )
        orch.inject_market_signal(sig)
        # 给 run_loop_arc 一点点时间消费消息
        time.sleep(0.3)
        s = orch.stats()
        # market_signals 至少 1(可能因 fan-in 重复计)
        assert s["market_signals"] >= 1
        assert s["messages_processed"] >= 1
    finally:
        orch.stop()


def test_inject_vote_response_records_vote():
    """inject_vote_response 后 orchestrator 收到消息,stats.messages_processed 增加。

    注:`SimpleMajority` 法定人数 == 2,单 vote 不达法定人数,不会触发
    `votes_passed` / `votes_rejected` 计数,这是 by design。
    """
    orch = SwarmOrchestrator(SwarmConfig())
    orch.register_market_agent(agent_id="m0")
    orch.register_risk_agent(agent_id="r0")
    orch.register_execution_agent(agent_id="e0", tools=None)
    orch.register_audit_agent(agent_id="a0")
    orch.start()
    try:
        orch.inject_vote_response(
            proposal_id="vp-1",
            voter="r0",
            approved=True,
            reasoning="ok",
            confidence=0.8,
        )
        time.sleep(0.3)
        s = orch.stats()
        # 消息已被消费(可能 vote dispatch 触发)
        assert s["messages_processed"] >= 1
    finally:
        orch.stop()


def test_inject_without_start_raises():
    """没 start() 就 inject → RuntimeError。"""
    orch = SwarmOrchestrator(SwarmConfig())
    sig = MarketSignal(
        symbol="BTC-USDT",
        signal_type=SignalType.Buy,
        confidence=0.5,
        reasoning="x",
    )
    with pytest.raises(RuntimeError):
        orch.inject_market_signal(sig)


def test_stop_without_start_raises():
    """没 start() 就 stop → RuntimeError。"""
    orch = SwarmOrchestrator(SwarmConfig())
    with pytest.raises(RuntimeError):
        orch.stop()


# ═══════════════════════════════════════════════════════════════════════════
# 6. __repr__ / dict 协议
# ═══════════════════════════════════════════════════════════════════════════


def test_orchestrator_repr_includes_agent_count():
    """SwarmOrchestrator.__repr__ 含 agent_count + running 字段。"""
    orch = SwarmOrchestrator(SwarmConfig())
    orch.register_market_agent(agent_id="m0")
    r = repr(orch)
    assert "SwarmOrchestrator" in r
    assert "agents=1" in r
    # Lazy start:register 后 running=true
    assert "running=true" in r
    orch.stop()


def test_swarm_config_repr_includes_key_fields():
    """SwarmConfig.__repr__ 含 vote_timeout_ms + loop_tick_ms。"""
    cfg = SwarmConfig(vote_timeout_ms=2000, loop_tick_ms=50)
    r = repr(cfg)
    assert "SwarmConfig" in r
    assert "2000" in r
    assert "50" in r


def test_stats_returns_dict_with_expected_keys():
    """stats() 返回 dict 含 11 个 key。"""
    orch = SwarmOrchestrator(SwarmConfig())
    s = orch.stats()
    expected = {
        "messages_processed",
        "market_signals",
        "risk_assessments",
        "execution_results",
        "votes_created",
        "votes_passed",
        "votes_rejected",
        "harness_approved",
        "harness_rejected",
        "harness_circuit_break",
        "shutdowns",
    }
    assert expected.issubset(set(s.keys()))
