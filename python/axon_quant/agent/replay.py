"""Trajectory 离线回放分析工具（0.11.0 E8）

用法:
    python -m axon_quant.agent.replay trajectory.json
    python -m axon_quant.agent.replay trajectory.json --bars 10 --step
    python -m axon_quant.agent.replay trajectory.json --agent t1 --show-tokens
    python -m axon_quant.agent.replay trajectory.json --export-csv out.csv
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path
from typing import Any

try:
    from rich.console import Console
    from rich.table import Table

    HAS_RICH = True
except ImportError:
    HAS_RICH = False


# ═══════════════════════════════════════════════════════════
# 3.4 加载
# ═══════════════════════════════════════════════════════════


def load_trajectory(path: str | Path) -> list[dict]:
    """加载 trajectory JSON → bar 列表。

    支持两种格式：
    - 顶层是 list[dict]（每 bar 一个 dict）
    - 顶层是 dict 且有 "bars" 字段
    """
    p = Path(path)
    if not p.exists():
        raise FileNotFoundError(f"trajectory file not found: {p}")

    with open(p) as f:
        data = json.load(f)

    if isinstance(data, list):
        return data
    if isinstance(data, dict) and "bars" in data:
        return data["bars"]
    raise ValueError(f"unsupported trajectory format: keys={list(data.keys()) if isinstance(data, dict) else type(data)}")


# ═══════════════════════════════════════════════════════════
# 3.5 逐 bar 回放
# ═══════════════════════════════════════════════════════════


def _format_bar_frame(idx: int, bar: dict, agent_filter: str | None, show_tokens: bool) -> str:
    """格式化单 bar 为终端文本（无 rich 依赖的 fallback）。"""
    lines = [f"{'═' * 60}", f"  Bar #{idx}"]

    # OHLCV
    symbol = bar.get("symbol", bar.get("bar_data", {}).get("symbol", "???"))
    close = bar.get("close", bar.get("bar_data", {}).get("close", 0))
    lines.append(f"  {symbol}  close={close}")

    # Votes
    votes = bar.get("votes", [])
    for v in votes:
        aid = v.get("agent_id", "?")
        if agent_filter and aid != agent_filter:
            continue
        action = v.get("action", "Hold")
        conf = v.get("confidence", 0.0)
        reasoning = v.get("reasoning", "")
        lines.append(f"    {aid}: {action:<4} (conf={conf:.2f})  \"{reasoning}\"")

    # Aggregated + Risk
    agg = bar.get("aggregated", {})
    risk = bar.get("risk_verdict", {})
    final = bar.get("final_action", "Hold")
    lines.append(f"  → {agg.get('action', '?')} (score={agg.get('score', 0):.2f}) | Risk: {'✓' if risk.get('approved', True) else '✗ ' + str(risk.get('reason', ''))}")
    lines.append(f"  Final: {final}")

    # Tokens
    if show_tokens:
        tok = bar.get("token_usage") or {}
        lines.append(f"  Tokens: in={tok.get('input_tokens', 0)} out={tok.get('output_tokens', 0)} cost=${tok.get('estimated_cost_usd', 0):.4f}")

    return "\n".join(lines)


def replay(
    bars: list[dict],
    max_bars: int | None = None,
    agent_filter: str | None = None,
    show_tokens: bool = False,
    step: bool = False,
    delay: float = 0.5,
):
    """逐 bar 回放。

    Args:
        bars: trajectory bar 列表
        max_bars: 最多回放 N 根（None=全部）
        agent_filter: 只显示指定 agent 的投票
        show_tokens: 显示 token 消耗
        step: 逐帧暂停（按 Enter 继续）
        delay: 自动播放间隔（秒）
    """
    subset = bars[:max_bars] if max_bars else bars
    total = len(subset)

    for i, bar in enumerate(subset):
        frame = _format_bar_frame(i, bar, agent_filter, show_tokens)
        print(frame)

        if step:
            try:
                input(f"  [{i + 1}/{total}] Enter to continue...")
            except (EOFError, KeyboardInterrupt):
                break
        elif i < total - 1:
            time.sleep(delay)

    print(f"\n{'═' * 60}")
    print(f"  Replay complete: {total} bars")


# ═══════════════════════════════════════════════════════════
# 3.6 汇总统计
# ═══════════════════════════════════════════════════════════


def compute_summary(bars: list[dict]) -> dict:
    """计算回放汇总统计。"""
    total = len(bars)
    buy_count = sum(1 for b in bars if b.get("final_action") == "Buy")
    sell_count = sum(1 for b in bars if b.get("final_action") == "Sell")
    hold_count = total - buy_count - sell_count
    vetoed = sum(1 for b in bars if not b.get("risk_verdict", {}).get("approved", True))

    # Token 汇总
    total_input = sum((b.get("token_usage") or {}).get("input_tokens", 0) for b in bars)
    total_output = sum((b.get("token_usage") or {}).get("output_tokens", 0) for b in bars)
    total_cost = sum((b.get("token_usage") or {}).get("estimated_cost_usd", 0.0) for b in bars)

    # Per-agent 命中率（action == final_action 视为命中）
    agent_stats: dict[str, dict] = {}
    for b in bars:
        final = b.get("final_action", "Hold")
        for v in b.get("votes", []):
            aid = v.get("agent_id", "?")
            if aid not in agent_stats:
                agent_stats[aid] = {"hits": 0, "total": 0}
            agent_stats[aid]["total"] += 1
            if v.get("action", "Hold") == final:
                agent_stats[aid]["hits"] += 1

    for aid in agent_stats:
        s = agent_stats[aid]
        s["hit_rate"] = s["hits"] / s["total"] if s["total"] > 0 else 0.0

    return {
        "total_bars": total,
        "buy_count": buy_count,
        "sell_count": sell_count,
        "hold_count": hold_count,
        "vetoed": vetoed,
        "total_tokens": total_input + total_output,
        "total_input_tokens": total_input,
        "total_output_tokens": total_output,
        "total_cost": total_cost,
        "agent_stats": agent_stats,
    }


def print_summary(stats: dict):
    """打印汇总统计。"""
    print(f"\n{'═' * 50}")
    print("  Run Summary")
    print(f"{'═' * 50}")
    print(f"  Total Bars:     {stats['total_bars']}")
    print(f"  Buy / Sell / Hold: {stats['buy_count']} / {stats['sell_count']} / {stats['hold_count']}")
    print(f"  Vetoed:         {stats['vetoed']}")
    print(f"  Total Tokens:   {stats['total_tokens']:,}")
    print(f"  Est. Cost:      ${stats['total_cost']:.4f}")
    if stats["agent_stats"]:
        print(f"  {'─' * 46}")
        for aid, s in stats["agent_stats"].items():
            print(f"  {aid}: hit_rate={s['hit_rate']:.1%} ({s['hits']}/{s['total']})")
    print(f"{'═' * 50}")


# ═══════════════════════════════════════════════════════════
# CLI 入口
# ═══════════════════════════════════════════════════════════


def main():
    parser = argparse.ArgumentParser(description="Trajectory 离线回放")
    parser.add_argument("trajectory", help="trajectory JSON 文件路径")
    parser.add_argument("--bars", type=int, default=None, help="最多回放 N 根 bar")
    parser.add_argument("--agent", type=str, default=None, help="只显示指定 agent")
    parser.add_argument("--show-tokens", action="store_true", help="显示 token 消耗")
    parser.add_argument("--step", action="store_true", help="逐帧暂停模式")
    parser.add_argument("--delay", type=float, default=0.5, help="自动播放间隔（秒）")
    parser.add_argument("--export-csv", type=str, default=None, help="导出 CSV 路径")
    parser.add_argument("--summary-only", action="store_true", help="只打印汇总，不回放")
    args = parser.parse_args()

    bars = load_trajectory(args.trajectory)
    print(f"Loaded {len(bars)} bars from {args.trajectory}")

    if args.export_csv:
        _export_csv(bars, args.export_csv)
        print(f"Exported to {args.export_csv}")

    if args.summary_only:
        stats = compute_summary(bars)
        print_summary(stats)
        return

    replay(
        bars,
        max_bars=args.bars,
        agent_filter=args.agent,
        show_tokens=args.show_tokens,
        step=args.step,
        delay=args.delay,
    )

    stats = compute_summary(bars[: args.bars] if args.bars else bars)
    print_summary(stats)


def _export_csv(bars: list[dict], path: str):
    """导出 bar 决策为 CSV。"""
    import csv

    with open(path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(["bar_index", "final_action", "final_confidence", "agg_action", "agg_score", "risk_approved", "risk_reason"])
        for i, b in enumerate(bars):
            agg = b.get("aggregated", {})
            risk = b.get("risk_verdict", {})
            writer.writerow([
                i,
                b.get("final_action", "Hold"),
                f"{b.get('final_confidence', 0):.3f}",
                agg.get("action", "Hold"),
                f"{agg.get('score', 0):.3f}",
                risk.get("approved", True),
                risk.get("reason", ""),
            ])


if __name__ == "__main__":
    main()
