"""50-bar LLM 交易主循环

实现:
1. 合成数据生成(随机游走价格)
2. Agent 创建(使用 MockProvider)
3. 50 bar 交易循环
4. Trajectory 记录与落盘
"""

from __future__ import annotations

import argparse
import json
import os
import random
from dataclasses import dataclass
from typing import Any, Dict, List, Optional


@dataclass
class BarData:
    bar_id: int
    ts: int
    open: float
    high: float
    low: float
    close: float
    volume: float


def generate_synthetic_data(seed: int, count: int = 50) -> List[BarData]:
    """生成合成 K 线数据(随机游走)"""
    rng = random.Random(seed)
    bars = []
    price = 50000.0
    base_ts = 1700000000000

    for i in range(count):
        change = rng.gauss(0, 0.001)
        new_price = price * (1 + change)
        high = max(price, new_price) * (1 + rng.random() * 0.0005)
        low = min(price, new_price) * (1 - rng.random() * 0.0005)
        open_price = price
        close_price = new_price
        volume = rng.uniform(100, 1000)

        bars.append(BarData(
            bar_id=i,
            ts=base_ts + i * 60000,
            open=open_price,
            high=high,
            low=low,
            close=close_price,
            volume=volume,
        ))
        price = new_price

    return bars


class MockProvider:
    """Mock LLM Provider"""

    def __init__(self, seed: int = 42):
        self.rng = random.Random(seed)

    def generate_response(self, prompt: str) -> Dict[str, Any]:
        """生成 mock 响应"""
        action_prob = self.rng.random()
        if action_prob < 0.3:
            return {
                "thought": "市场上涨趋势，买入",
                "action": {
                    "tool": "place_order",
                    "args": {
                        "symbol": "BTC-USDT",
                        "side": "Buy",
                        "quantity": round(self.rng.uniform(0.01, 0.1), 4),
                        "price": round(50000 + self.rng.gauss(0, 500), 2),
                    },
                },
            }
        elif action_prob < 0.6:
            return {
                "thought": "市场下跌趋势，卖出",
                "action": {
                    "tool": "place_order",
                    "args": {
                        "symbol": "BTC-USDT",
                        "side": "Sell",
                        "quantity": round(self.rng.uniform(0.01, 0.1), 4),
                        "price": round(50000 + self.rng.gauss(0, 500), 2),
                    },
                },
            }
        else:
            return {
                "thought": "观望，不操作",
                "action": None,
            }


class TradingLoop:
    """50-bar 交易循环"""

    def __init__(
        self,
        seed: int = 42,
        output_dir: str = "output",
        instrument: str = "BTC-USDT",
    ):
        self.seed = seed
        self.output_dir = output_dir
        self.instrument = instrument
        self.provider = MockProvider(seed)
        self.cash = 100000.0
        self.position = 0.0
        self.pnl = 0.0
        self.trade_count = 0

    def run(self) -> Dict[str, Any]:
        """运行 50-bar 交易循环"""
        os.makedirs(self.output_dir, exist_ok=True)

        bars = generate_synthetic_data(self.seed)
        trajectory = {
            "version": "0.10.0",
            "run_id": f"run-{self.seed}",
            "instrument": self.instrument,
            "provider": "mock",
            "model": "mock-model",
            "seed": self.seed,
            "bars": [],
            "summary": None,
        }

        for bar in bars:
            response = self.provider.generate_response("")
            thought = response["thought"]
            action = response["action"]

            reward = 0.0
            observation = None

            if action and action["tool"] == "place_order":
                args = action["args"]
                side = args["side"]
                quantity = args["quantity"]
                price = args["price"]
                notional = quantity * price

                if side == "Buy" and self.cash >= notional:
                    self.cash -= notional
                    self.position += quantity
                    reward = self.provider.rng.gauss(0, 10)
                    observation = f"Bought {quantity} @ {price}"
                    self.trade_count += 1
                elif side == "Sell" and self.position >= quantity:
                    self.cash += notional
                    self.position -= quantity
                    reward = self.provider.rng.gauss(0, 10)
                    observation = f"Sold {quantity} @ {price}"
                    self.trade_count += 1
                else:
                    observation = "Insufficient funds or position"

            self.pnl += reward

            bar_record = {
                "bar_id": bar.bar_id,
                "ts": bar.ts,
                "thought": thought,
                "action": action,
                "observation": observation,
                "reward": reward,
                "cum_pnl": self.pnl,
            }
            trajectory["bars"].append(bar_record)

        final_pnl = self.cash + self.position * bars[-1].close - 100000.0
        trajectory["summary"] = {
            "total_pnl": final_pnl,
            "trades": self.trade_count,
            "final_position": self.position,
            "wall_time_s": 0.0,
            "cost_usd": 0.0,
        }

        output_path = os.path.join(self.output_dir, f"trajectory_{self.seed}.json")
        with open(output_path, "w") as f:
            json.dump(trajectory, f, indent=2)

        print(f"Trajectory saved to {output_path}")
        print(f"Final PnL: {final_pnl:.2f}")
        print(f"Total trades: {self.trade_count}")

        return trajectory


def main():
    parser = argparse.ArgumentParser(description="Run 50-bar LLM trading simulation")
    parser.add_argument("--seed", type=int, default=42, help="Random seed")
    parser.add_argument("--output", type=str, default="output", help="Output directory")
    parser.add_argument("--instrument", type=str, default="BTC-USDT", help="Trading instrument")

    args = parser.parse_args()

    loop = TradingLoop(
        seed=args.seed,
        output_dir=args.output,
        instrument=args.instrument,
    )
    loop.run()


if __name__ == "__main__":
    main()