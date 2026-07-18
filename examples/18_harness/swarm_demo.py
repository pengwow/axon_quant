#!/usr/bin/env python3
"""AXON Swarm 4-Agent Pipeline Demo(0.6.0 P0 工作流 B 收口)。

演示内容:
  1. SwarmOrchestrator 构造 + 4 类 agent 注册(Market / Risk / Execution / Audit)
  2. ExecutionAgent 接 MockTradingBackend + PlaceOrderTool + QueryPortfolioTool
  3. inject_market_signal 触发 MarketAnalysis → 投票 → Execution
  4. 读 stats() 观察各阶段计数
  5. inject_vote_response 主动投票
  6. 关闭 orchestrator

运行方式:
    source .venv/bin/activate
    python examples/18_harness/swarm_demo.py
"""

from __future__ import annotations

import time

from axon_quant.llm import (
    MarketSignal,
    SignalType,
    SwarmConfig,
    SwarmOrchestrator,
    TradingTools,
)
from axon_quant.trading import (
    MockTradingBackend,
    PlaceOrderTool,
    QueryPortfolioTool,
    RiskLimits,
)


def main() -> None:
    print("=" * 70)
    print("AXON Swarm 4-Agent Pipeline Demo")
    print("=" * 70)

    # ─── 1. 构造 TradingBackend + Tools ────────────────────────────────
    print("\n[1] 构造 MockTradingBackend + PlaceOrderTool + QueryPortfolioTool")
    backend = MockTradingBackend()
    risk_limits = RiskLimits(allowed_symbols=["BTC-USDT"], max_order_notional=100_000.0)
    place = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk_limits)
    query = QueryPortfolioTool(backend=backend)
    tools = TradingTools(place_order=place, query_portfolio=query)
    print(f"  backend = {backend!r}")
    print(f"  tools   = {tools!r}")

    # ─── 2. 构造 SwarmOrchestrator ─────────────────────────────────────
    print("\n[2] 构造 SwarmOrchestrator")
    config = SwarmConfig(vote_timeout_ms=3000, loop_tick_ms=50)
    orch = SwarmOrchestrator(config)
    print(f"  config  = {config!r}")
    print(f"  orch    = {orch!r}")

    # ─── 3. 注册 4 类 agent ────────────────────────────────────────────
    print("\n[3] 注册 4 类 agent(Market / Risk / Execution / Audit)")
    orch.register_market_agent(
        agent_id="market_0",
        symbols=["BTC-USDT", "ETH-USDT"],
        price_change_threshold=0.7,
    )
    print(f"  registered: market_0 → {orch.agent_status('market_0')!r}")

    orch.register_risk_agent(agent_id="risk_0")
    print(f"  registered: risk_0   → {orch.agent_status('risk_0')!r}")

    orch.register_execution_agent(agent_id="execution_0", tools=tools)
    print(f"  registered: execution_0 → {orch.agent_status('execution_0')!r}")

    orch.register_audit_agent(agent_id="audit_0")
    print(f"  registered: audit_0   → {orch.agent_status('audit_0')!r}")

    print(f"  total agents: {orch.agent_count()}")
    print(f"  running     : {orch.is_running()}")

    # ─── 4. 注入 MarketSignal ──────────────────────────────────────────
    print("\n[4] 注入 MarketSignal(BTC-USDT Buy,confidence=0.9)")
    sig = MarketSignal(
        symbol="BTC-USDT",
        signal_type=SignalType.Buy,
        confidence=0.9,
        reasoning="momentum breakout + RSI oversold recovery",
    )
    orch.inject_market_signal(sig)
    print(f"  signal = {sig!r}")

    # 等 orchestrator 消费消息
    time.sleep(0.5)
    stats = orch.stats()
    print(f"  stats  = {stats}")

    # ─── 5. 注入另一个 MarketSignal(ETH-USDT Sell) ─────────────────────
    print("\n[5] 注入 MarketSignal(ETH-USDT Sell,confidence=0.6)")
    sig2 = MarketSignal(
        symbol="ETH-USDT",
        signal_type=SignalType.Sell,
        confidence=0.6,
        reasoning="overhead resistance rejection",
    )
    orch.inject_market_signal(sig2)
    print(f"  signal = {sig2!r}")
    time.sleep(0.5)
    stats = orch.stats()
    print(f"  stats  = {stats}")

    # ─── 6. 主动投票(达成 SimpleMajority 法定人数 = 2) ─────────────────
    print("\n[6] 主动投票(2 个 voter 投 yes,触发 VoteResult)")
    orch.inject_vote_response(
        proposal_id="vp_demo_1",
        voter="risk_0",
        approved=True,
        reasoning="risk ok",
        confidence=0.85,
    )
    orch.inject_vote_response(
        proposal_id="vp_demo_1",
        voter="market_0",
        approved=True,
        reasoning="signal confirmed",
        confidence=0.9,
    )
    time.sleep(0.5)
    stats = orch.stats()
    print(f"  stats  = {stats}")

    # ─── 7. 创建投票(不投递,仅验证 API) ─────────────────────────────
    print("\n[7] create_vote API 验证")
    from axon_quant.llm import VoteProposal, VoteType

    pid = orch.create_vote(
        VoteProposal(
            proposal_id="vp_demo_2",
            proposal_type=VoteType.StrategyAdjustment,
            content="Tighten stop-loss threshold",
            deadline_ms=5000,
        )
    )
    print(f"  created proposal: {pid}")
    stats = orch.stats()
    print(f"  stats  = {stats}")

    # ─── 8. 关闭 orchestrator ──────────────────────────────────────────
    print("\n[8] 关闭 orchestrator")
    orch.stop()
    print(f"  is_running = {orch.is_running()}")
    print(f"  final repr = {orch!r}")

    # ─── 9. 验证 MockBackend 收单 ──────────────────────────────────────
    print("\n[9] MockBackend 状态")
    print(f"  order_count = {backend.order_count()}")

    print("\n" + "=" * 70)
    print("Done.")
    print("=" * 70)


if __name__ == "__main__":
    main()
