"""Multi-agent 投票共识 CLI 实时面板（0.11.0 E8）

用法:
    from axon_quant.agent.cli import SwarmPanel

    panel = SwarmPanel()
    panel.render(bar_index=42, bar_data={...}, decision={...})
    # 或作为 context manager 使用 Live 模式
"""

from __future__ import annotations

import json
from typing import Any

try:
    from rich.console import Console
    from rich.live import Live
    from rich.panel import Panel
    from rich.table import Table
    from rich.text import Text

    HAS_RICH = True
except ImportError:
    HAS_RICH = False


def _require_rich():
    if not HAS_RICH:
        raise ImportError("rich is required: pip install axon-quant[cli]")


class SwarmPanel:
    """实时决策面板，渲染 multi-agent 投票过程。"""

    def __init__(self, console: Any | None = None):
        _require_rich()
        self.console = console or Console()
        self._live: Live | None = None

    def render_frame(self, bar_index: int, bar_data: dict, decision: dict) -> Panel:
        """渲染单帧决策面板。"""
        table = Table(show_header=False, box=None, padding=(0, 1))
        table.add_column("Content", ratio=1)

        # Bar 数据行
        symbol = bar_data.get("symbol", "???")
        o = bar_data.get("open", 0)
        h = bar_data.get("high", 0)
        low = bar_data.get("low", 0)
        c = bar_data.get("close", 0)
        table.add_row(f"[bold]{symbol}[/bold]  O:{o}  H:{h}  L:{low}  C:{c}")
        table.add_row("─" * 56)

        # 各 trader 投票
        votes = decision.get("votes", [])
        for v in votes:
            action = v.get("action", "Hold")
            conf = v.get("confidence", 0.0)
            reasoning = v.get("reasoning", "")
            agent_id = v.get("agent_id", "?")
            color = {"Buy": "green", "Sell": "red", "Hold": "yellow"}.get(action, "white")
            table.add_row(
                f"  {agent_id}: [{color}]{action:<4}[/{color}] (conf={conf:.2f})  \"{reasoning}\""
            )

        table.add_row("─" * 56)

        # 聚合 + Risk
        agg = decision.get("aggregated", {})
        agg_action = agg.get("action", "Hold")
        agg_score = agg.get("score", 0.0)
        risk = decision.get("risk_verdict", {})
        approved = risk.get("approved", True)
        reason = risk.get("reason")

        if approved:
            risk_str = "[green]✓ APPROVED[/green]"
        else:
            risk_str = f"[red]✗ VETOED[/red] ({reason})"

        table.add_row(f"  Vote: {agg_action} (score={agg_score:.2f})  │  Risk: {risk_str}")

        # Final action + token
        final = decision.get("final_action", "Hold")
        final_conf = decision.get("final_confidence", 0.0)
        tokens = decision.get("token_usage") or {}
        total_tok = tokens.get("input_tokens", 0) + tokens.get("output_tokens", 0)
        cost = tokens.get("estimated_cost_usd", 0.0)
        table.add_row(
            f"  Action: [bold]{final}[/bold] (conf={final_conf:.2f}) │  Tokens: {total_tok:,} (${cost:.4f})"
        )

        title = f"Bar #{bar_index}"
        return Panel(table, title=title, border_style="blue")

    def print_frame(self, bar_index: int, bar_data: dict, decision: dict):
        """打印单帧（非 Live 模式）。"""
        frame = self.render_frame(bar_index, bar_data, decision)
        self.console.print(frame)

    def run_live(self, decisions_iter):
        """Live 模式：迭代 decisions_iter，每帧刷新。

        decisions_iter: yields (bar_index, bar_data, decision) tuples
        """
        _require_rich()
        with Live(console=self.console, refresh_per_second=4) as live:
            for bar_index, bar_data, decision in decisions_iter:
                frame = self.render_frame(bar_index, bar_data, decision)
                live.update(frame)


def format_summary(stats: dict) -> str:
    """格式化汇总统计为终端输出。"""
    _require_rich()
    lines = [
        "═" * 50,
        "  Run Summary",
        "═" * 50,
        f"  Total PnL:      {stats.get('total_pnl', 0):.2f}",
        f"  Win Rate:       {stats.get('win_rate', 0):.1%}",
        f"  Total Bars:     {stats.get('total_bars', 0)}",
        f"  Trades:         {stats.get('trades', 0)}",
        f"  Vetoed:         {stats.get('vetoed', 0)}",
        f"  Total Tokens:   {stats.get('total_tokens', 0):,}",
        f"  Est. Cost:      ${stats.get('total_cost', 0):.4f}",
    ]
    # Per-agent hit rate
    agent_stats = stats.get("agent_stats", {})
    if agent_stats:
        lines.append("  ─" * 25)
        for agent_id, agent_stat in agent_stats.items():
            hit = agent_stat.get("hit_rate", 0)
            lines.append(f"  {agent_id}: hit_rate={hit:.1%}")
    lines.append("═" * 50)
    return "\n".join(lines)
