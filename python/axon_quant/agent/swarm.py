"""Multi-Agent 投票共识高层封装（0.11.0）

用法:
    from axon_quant.agent.swarm import SwarmRunner, MockTrader

    traders = [MockTrader("t1", "buy"), MockTrader("t2", "buy"), MockTrader("t3", "hold")]
    runner = SwarmRunner(traders, voting="weighted_majority")
    results = runner.run(df, symbol="BTCUSDT")
"""

from __future__ import annotations

import random
from dataclasses import dataclass, field
from typing import Any, Callable

try:
    import pandas as pd
except ImportError:
    pd = None  # type: ignore[assignment]


# ═══════════════════════════════════════════════════════════
# Trader 协议 + Mock 实现
# ═══════════════════════════════════════════════════════════


class MockTrader:
    """固定动作的 Mock Trader（测试用）。

    Attributes:
        id: agent 标识
        action: 固定输出动作 ("buy"/"sell"/"hold")
        confidence: 固定置信度
    """

    def __init__(self, id: str, action: str = "hold", confidence: float = 0.5):
        self.id = id
        self._action = action
        self._confidence = confidence

    def decide(self, bar: dict) -> dict:
        return {
            "action": self._action,
            "confidence": self._confidence,
            "reasoning": f"mock {self.id}: fixed {self._action}",
        }


class RandomTrader:
    """随机决策 Trader（演示/压力测试用）。"""

    def __init__(self, id: str, seed: int | None = None):
        self.id = id
        self._rng = random.Random(seed)

    def decide(self, bar: dict) -> dict:
        action = self._rng.choice(["buy", "sell", "hold"])
        conf = round(self._rng.uniform(0.3, 0.9), 2) if action != "hold" else 0.0
        return {
            "action": action,
            "confidence": conf,
            "reasoning": f"random {self.id}: {action}",
        }


class RuleTrader:
    """基于简单均线规则的 Trader。

    维护 close 历史，fast > slow → buy，fast < slow → sell。
    """

    def __init__(self, id: str, fast: int = 5, slow: int = 20):
        self.id = id
        self.fast = fast
        self.slow = slow
        self._closes: list[float] = []

    def decide(self, bar: dict) -> dict:
        close = bar.get("close", 0.0)
        self._closes.append(close)
        if len(self._closes) < self.slow:
            return {"action": "hold", "confidence": 0.0, "reasoning": f"{self.id}: warming up"}
        fast_ma = sum(self._closes[-self.fast :]) / self.fast
        slow_ma = sum(self._closes[-self.slow :]) / self.slow
        if fast_ma > slow_ma:
            return {"action": "buy", "confidence": 0.7, "reasoning": f"{self.id}: fast>slow MA"}
        elif fast_ma < slow_ma:
            return {"action": "sell", "confidence": 0.7, "reasoning": f"{self.id}: fast<slow MA"}
        return {"action": "hold", "confidence": 0.0, "reasoning": f"{self.id}: MA flat"}


# ═══════════════════════════════════════════════════════════
# SwarmRunner — 高层批量执行
# ═══════════════════════════════════════════════════════════


@dataclass
class SwarmResult:
    """单次 run 的汇总结果。"""

    decisions: list[dict] = field(default_factory=list)
    total_bars: int = 0
    buy_count: int = 0
    sell_count: int = 0
    hold_count: int = 0
    vetoed_count: int = 0


class SwarmRunner:
    """Multi-agent 投票共识运行器。

    优先使用 Rust 原生 VotingOrchestrator（通过 PyO3），
    若 native 不可用则 fallback 到纯 Python 实现。
    """

    def __init__(
        self,
        traders: list[Any],
        risk_config: dict | None = None,
        voting: str = "weighted_majority",
        use_native: bool = True,
    ):
        self.traders = traders
        self.risk_config = risk_config or {}
        self.voting = voting
        self._native = None

        if use_native:
            try:
                from axon_quant._native.llm.swarm import VotingOrchestrator

                self._native = VotingOrchestrator(
                    traders=traders,
                    risk_config=self.risk_config,
                    voting=voting,
                )
            except (ImportError, Exception):
                self._native = None

    def on_bar(self, bar: dict) -> dict:
        """处理单根 bar，返回决策 dict。"""
        if self._native is not None:
            return self._native.on_bar(bar)
        return self._on_bar_python(bar)

    def run(
        self,
        data: "pd.DataFrame",
        symbol: str = "BTCUSDT",
        on_decision: "Callable[[int, dict, dict], None] | None" = None,
    ) -> SwarmResult:
        """批量执行 DataFrame 中的所有 bar。

        Args:
            data: 含 open/high/low/close/volume 列的 DataFrame
            symbol: 交易对名
            on_decision: 可选回调 (bar_index, bar_dict, decision_dict)，用于 CLI 实时渲染

        Returns:
            SwarmResult 汇总
        """
        if pd is None:
            raise ImportError("pandas is required: pip install pandas")

        result = SwarmResult()
        records = data.to_dict("records")

        for i, row in enumerate(records):
            bar = {
                "open": float(row.get("open", 0)),
                "high": float(row.get("high", 0)),
                "low": float(row.get("low", 0)),
                "close": float(row.get("close", 0)),
                "volume": float(row.get("volume", 0)),
                "symbol": symbol,
            }
            decision = self.on_bar(bar)
            result.decisions.append(decision)

            # 统计
            action = decision.get("final_action", "Hold")
            if action == "Buy":
                result.buy_count += 1
            elif action == "Sell":
                result.sell_count += 1
            else:
                result.hold_count += 1
            if not decision.get("risk_verdict", {}).get("approved", True):
                result.vetoed_count += 1

            # CLI hook
            if on_decision is not None:
                on_decision(i, bar, decision)

        result.total_bars = len(records)
        return result

    # ─── 纯 Python fallback ───

    def _on_bar_python(self, bar: dict) -> dict:
        """纯 Python 投票共识逻辑（native 不可用时的 fallback）。"""
        votes = []
        for t in self.traders:
            v = t.decide(bar)
            votes.append(
                {
                    "agent_id": getattr(t, "id", "?"),
                    "action": v.get("action", "hold").capitalize(),
                    "confidence": v.get("confidence", 0.0),
                    "reasoning": v.get("reasoning", ""),
                }
            )

        # 加权聚合
        buy_score = sum(v["confidence"] for v in votes if v["action"] == "Buy")
        sell_score = sum(v["confidence"] for v in votes if v["action"] == "Sell")
        hold_score = sum(max(v["confidence"], 0.1) for v in votes if v["action"] == "Hold")
        total = buy_score + sell_score + hold_score

        if buy_score >= sell_score and buy_score >= hold_score:
            agg_action, agg_score = "Buy", buy_score
        elif sell_score >= buy_score and sell_score >= hold_score:
            agg_action, agg_score = "Sell", sell_score
        else:
            agg_action, agg_score = "Hold", hold_score

        normalized = agg_score / total if total > 0 else 0.0
        threshold = 0.5
        if normalized < threshold:
            agg_action = "Hold"

        aggregated = {"action": agg_action, "score": normalized, "strategy": self.voting}

        # Risk（简化版）
        max_pos = self.risk_config.get("max_position", 1.0)
        approved = True
        reason = None
        if agg_action != "Hold" and normalized > max_pos:
            approved = False
            reason = f"score {normalized:.2f} exceeds max_position {max_pos}"

        risk_verdict = {"approved": approved, "reason": reason}
        final_action = agg_action if approved else "Hold"
        final_conf = normalized if approved else 0.0

        return {
            "final_action": final_action,
            "final_confidence": final_conf,
            "votes": votes,
            "aggregated": aggregated,
            "risk_verdict": risk_verdict,
            "token_usage": None,
        }
