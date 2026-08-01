"""LLM Agent Module

提供基于 ReAct 模式的交易 Agent 框架。
"""

from __future__ import annotations

from .react import ReActAgent
from .swarm import MockTrader, RandomTrader, RuleTrader, SwarmRunner, SwarmResult
from .tools import TradingTools
from .trajectory import TrajectoryRecorder

__all__ = [
    "ReActAgent",
    "TradingTools",
    "TrajectoryRecorder",
    "MockTrader",
    "RandomTrader",
    "RuleTrader",
    "SwarmRunner",
    "SwarmResult",
]