#!/usr/bin/env python3
"""0.11.0 主验收脚本：Multi-Agent 投票共识 50 bar 演示

用法:
    python examples/llm_trading/run_swarm_50bar.py
    python examples/llm_trading/run_swarm_50bar.py --traders 5 --bars 100
    python examples/llm_trading/run_swarm_50bar.py --voting unanimous --show-tokens

输出:
    - 终端 CLI 面板（实时决策）
    - trajectory JSON 文件
    - token report + 汇总统计
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

# 确保能导入 axon_quant（开发模式）
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "python"))

# 直接导入 swarm 模块（绕过 _native 依赖）
import importlib.util as _ilu

_swarm_path = Path(__file__).resolve().parents[2] / "python" / "axon_quant" / "agent" / "swarm.py"
_spec = _ilu.spec_from_file_location("axon_quant.agent.swarm", _swarm_path)
_swarm_mod = _ilu.module_from_spec(_spec)
sys.modules["axon_quant.agent.swarm"] = _swarm_mod
_spec.loader.exec_module(_swarm_mod)

MockTrader = _swarm_mod.MockTrader
RandomTrader = _swarm_mod.RandomTrader
RuleTrader = _swarm_mod.RuleTrader
SwarmRunner = _swarm_mod.SwarmRunner


def generate_bars(n: int, start_price: float = 67000.0) -> list[dict]:
    """生成模拟 bar 数据。"""
    import random

    rng = random.Random(42)
    bars = []
    price = start_price
    for _ in range(n):
        change = rng.uniform(-200, 200)
        o = price
        c = price + change
        h = max(o, c) + rng.uniform(0, 100)
        low = min(o, c) - rng.uniform(0, 100)
        bars.append({
            "open": round(o, 2),
            "high": round(h, 2),
            "low": round(low, 2),
            "close": round(c, 2),
            "volume": round(rng.uniform(50, 500), 2),
            "symbol": "BTCUSDT",
        })
        price = c
    return bars


def main():
    parser = argparse.ArgumentParser(description="Multi-Agent Swarm 50-bar Demo")
    parser.add_argument("--traders", type=int, default=3, help="Trader 数量")
    parser.add_argument("--bars", type=int, default=50, help="Bar 数量")
    parser.add_argument("--voting", type=str, default="weighted_majority", choices=["weighted_majority", "unanimous"])
    parser.add_argument("--provider", type=str, default="mock", choices=["mock", "random", "rule"])
    parser.add_argument("--show-tokens", action="store_true")
    parser.add_argument("--output", type=str, default=None, help="trajectory 输出路径")
    parser.add_argument("--no-cli", action="store_true", help="禁用 CLI 面板")
    args = parser.parse_args()

    print(f"{'═' * 60}")
    print(f"  axon_quant 0.11.0 — Multi-Agent Voting Consensus Demo")
    print(f"  Traders: {args.traders} | Bars: {args.bars} | Voting: {args.voting} | Provider: {args.provider}")
    print(f"{'═' * 60}\n")

    # 构造 traders
    if args.provider == "mock":
        traders = [MockTrader(f"t{i}", "buy" if i % 2 == 0 else "hold", 0.7) for i in range(args.traders)]
    elif args.provider == "random":
        traders = [RandomTrader(f"t{i}", seed=i) for i in range(args.traders)]
    else:  # rule
        traders = [RuleTrader(f"t{i}", fast=3 + i * 2, slow=10 + i * 5) for i in range(args.traders)]

    # 构造 runner
    runner = SwarmRunner(
        traders=traders,
        risk_config={"max_position": 2.0, "max_consecutive_loss": 10, "max_drawdown": 0.5},
        voting=args.voting,
        use_native=False,  # 纯 Python（native 需要编译）
    )

    # 生成 bar 数据
    bars = generate_bars(args.bars)

    # CLI 面板
    panel = None
    if not args.no_cli:
        try:
            _cli_path = Path(__file__).resolve().parents[2] / "python" / "axon_quant" / "agent" / "cli.py"
            _cli_spec = _ilu.spec_from_file_location("axon_quant.agent.cli", _cli_path)
            _cli_mod = _ilu.module_from_spec(_cli_spec)
            _cli_spec.loader.exec_module(_cli_mod)
            panel = _cli_mod.SwarmPanel()
        except (ImportError, Exception):
            pass

    # 执行
    decisions = []
    start = time.time()

    def on_decision(idx, bar_data, decision):
        decisions.append(decision)
        if panel:
            panel.print_frame(idx, bar_data, decision)

    import pandas as pd
    df = pd.DataFrame(bars)
    result = runner.run(df, symbol="BTCUSDT", on_decision=on_decision)
    elapsed = time.time() - start

    # 汇总
    print(f"\n{'═' * 60}")
    print(f"  Results ({elapsed:.3f}s)")
    print(f"{'═' * 60}")
    print(f"  Total Bars:  {result.total_bars}")
    print(f"  Buy:         {result.buy_count}")
    print(f"  Sell:        {result.sell_count}")
    print(f"  Hold:        {result.hold_count}")
    print(f"  Vetoed:      {result.vetoed_count}")
    print(f"{'═' * 60}")

    # 输出 trajectory
    output_path = args.output or f"trajectory_swarm_{args.bars}bar.json"
    trajectory_data = []
    for i, (bar, dec) in enumerate(zip(bars, decisions)):
        entry = {**dec, "bar_data": bar, "bar_index": i}
        trajectory_data.append(entry)

    Path(output_path).write_text(json.dumps(trajectory_data, indent=2, ensure_ascii=False))
    print(f"\n  Trajectory saved: {output_path}")
    print(f"  Replay: python -m axon_quant.agent.replay {output_path} --show-tokens")


if __name__ == "__main__":
    main()
