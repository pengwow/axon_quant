"""Rule-Based Mock Provider

规则引擎实现:
1. 简单移动平均线(SMA)交叉规则
2. RSI 超买超卖规则
3. 随机游走噪声
"""

from __future__ import annotations

import random
from typing import Any, Dict, List, Optional


class RuleBasedMockProvider:
    """基于规则的 Mock LLM Provider"""

    def __init__(self, seed: int = 42, sma_short: int = 5, sma_long: int = 20):
        self.rng = random.Random(seed)
        self.sma_short = sma_short
        self.sma_long = sma_long
        self.price_history: List[float] = []
        self.position: float = 0.0
        self.cash: float = 100000.0

    def _calculate_sma(self, prices: List[float], period: int) -> float:
        """计算简单移动平均线"""
        if len(prices) < period:
            return prices[-1] if prices else 50000.0
        return sum(prices[-period:]) / period

    def _calculate_rsi(self, prices: List[float], period: int = 14) -> float:
        """计算 RSI"""
        if len(prices) < period + 1:
            return 50.0

        deltas = [prices[i] - prices[i - 1] for i in range(1, len(prices))]
        gains = [d if d > 0 else 0 for d in deltas[-period:]]
        losses = [-d if d < 0 else 0 for d in deltas[-period:]]

        avg_gain = sum(gains) / period
        avg_loss = sum(losses) / period

        if avg_loss == 0:
            return 100.0
        rs = avg_gain / avg_loss
        return 100 - (100 / (1 + rs))

    def generate_response(self, prompt: str) -> Dict[str, Any]:
        """生成基于规则的响应"""
        noise_prob = self.rng.random()
        if noise_prob < 0.2:
            return self._generate_noise_action()

        if len(self.price_history) < self.sma_long:
            return self._generate_noise_action()

        sma_short_val = self._calculate_sma(self.price_history, self.sma_short)
        sma_long_val = self._calculate_sma(self.price_history, self.sma_long)
        rsi = self._calculate_rsi(self.price_history)

        return self._apply_rules(sma_short_val, sma_long_val, rsi)

    def _generate_noise_action(self) -> Dict[str, Any]:
        """生成随机噪声动作"""
        action_prob = self.rng.random()
        if action_prob < 0.3:
            return {
                "thought": "随机买入",
                "action": {
                    "tool": "place_order",
                    "args": {
                        "symbol": "BTC-USDT",
                        "side": "Buy",
                        "quantity": round(self.rng.uniform(0.01, 0.05), 4),
                        "price": round(50000 + self.rng.gauss(0, 200), 2),
                    },
                },
            }
        elif action_prob < 0.6:
            return {
                "thought": "随机卖出",
                "action": {
                    "tool": "place_order",
                    "args": {
                        "symbol": "BTC-USDT",
                        "side": "Sell",
                        "quantity": round(self.rng.uniform(0.01, 0.05), 4),
                        "price": round(50000 + self.rng.gauss(0, 200), 2),
                    },
                },
            }
        else:
            return {
                "thought": "随机观望",
                "action": None,
            }

    def _apply_rules(self, sma_short: float, sma_long: float, rsi: float) -> Dict[str, Any]:
        """应用交易规则"""
        if sma_short > sma_long and rsi < 70:
            return {
                "thought": f"SMA金叉(短{self.sma_short}>长{self.sma_long}),RSI={rsi:.1f},买入",
                "action": {
                    "tool": "place_order",
                    "args": {
                        "symbol": "BTC-USDT",
                        "side": "Buy",
                        "quantity": round(self.rng.uniform(0.02, 0.1), 4),
                        "price": round(self.price_history[-1] * (1 + self.rng.uniform(0, 0.001)), 2),
                    },
                },
            }
        elif sma_short < sma_long and rsi > 30:
            return {
                "thought": f"SMA死叉(短{self.sma_short}<长{self.sma_long}),RSI={rsi:.1f},卖出",
                "action": {
                    "tool": "place_order",
                    "args": {
                        "symbol": "BTC-USDT",
                        "side": "Sell",
                        "quantity": round(self.rng.uniform(0.02, 0.1), 4),
                        "price": round(self.price_history[-1] * (1 - self.rng.uniform(0, 0.001)), 2),
                    },
                },
            }
        elif rsi > 75:
            return {
                "thought": f"RSI超买({rsi:.1f}>75),卖出",
                "action": {
                    "tool": "place_order",
                    "args": {
                        "symbol": "BTC-USDT",
                        "side": "Sell",
                        "quantity": round(self.rng.uniform(0.02, 0.08), 4),
                        "price": round(self.price_history[-1] * (1 - self.rng.uniform(0, 0.002)), 2),
                    },
                },
            }
        elif rsi < 25:
            return {
                "thought": f"RSI超卖({rsi:.1f}<25),买入",
                "action": {
                    "tool": "place_order",
                    "args": {
                        "symbol": "BTC-USDT",
                        "side": "Buy",
                        "quantity": round(self.rng.uniform(0.02, 0.08), 4),
                        "price": round(self.price_history[-1] * (1 + self.rng.uniform(0, 0.002)), 2),
                    },
                },
            }
        else:
            return {
                "thought": f"SMA={sma_short:.2f}/{sma_long:.2f},RSI={rsi:.1f},观望",
                "action": None,
            }

    def update_price(self, price: float) -> None:
        """更新价格历史"""
        self.price_history.append(price)