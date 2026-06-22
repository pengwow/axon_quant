#!/usr/bin/env python3
"""AXON Quant 传统量化策略演示 —— 三大经典策略的信号生成与回测模拟。

覆盖:
  1. SimpleMomentumStrategy —— 双均线交叉 + 成交量确认
  2. MeanReversionStrategy —— 布林带 + RSI 超买超卖
  3. TrendFollowingStrategy —— 均线突破 + ATR 止损

运行方式:
    source .venv/bin/activate
    python examples/16_traditional_strategy/traditional_strategy_demo.py

零外部依赖: 仅使用 Python 标准库。
"""

from __future__ import annotations

import math
import random
import sys
from dataclasses import dataclass, field
from typing import Any

RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
RED = "\033[31m"
GREEN = "\033[32m"
YELLOW = "\033[33m"
CYAN = "\033[36m"
MAGENTA = "\033[35m"

if sys.platform == "win32":
    try:
        import os
        os.system("")
    except Exception:
        pass


def header(title: str, icon: str = "▶") -> None:
    print(f"\n{BOLD}{CYAN}{'═' * 60}{RESET}")
    print(f"{BOLD}{CYAN}  {icon} {title}{RESET}")
    print(f"{BOLD}{CYAN}{'═' * 60}{RESET}")


def step(n: int, text: str) -> None:
    print(f"\n  {BOLD}{YELLOW}[步骤 {n}]{RESET} {text}")


def ok(msg: str) -> None:
    print(f"    {GREEN}✅ {msg}{RESET}")


def info(msg: str) -> None:
    print(f"    {DIM}{msg}{RESET}")


def warn(msg: str) -> None:
    print(f"    {YELLOW}⚠️  {msg}{RESET}")


def value(label: str, v: Any, width: int = 20) -> None:
    print(f"    {BOLD}{label:<{width}}{RESET} {v}")


def separator() -> None:
    print(f"    {DIM}{'─' * 50}{RESET}")


def pnl_color(val: float) -> str:
    if val > 0:
        return f"{GREEN}+{val:,.2f}{RESET}"
    elif val < 0:
        return f"{RED}{val:,.2f}{RESET}"
    return f"{val:,.2f}"


@dataclass
class Bar:
    timestamp: int
    open: float
    high: float
    low: float
    close: float
    volume: float


@dataclass
class Trade:
    entry_price: float
    exit_price: float
    side: str
    quantity: float
    entry_bar: int
    exit_bar: int

    @property
    def pnl(self) -> float:
        if self.side == "Buy":
            return (self.exit_price - self.entry_price) * self.quantity
        return (self.entry_price - self.exit_price) * self.quantity

    @property
    def is_win(self) -> bool:
        return self.pnl > 0


@dataclass
class BacktestResult:
    trades: list[Trade] = field(default_factory=list)

    @property
    def total_pnl(self) -> float:
        return sum(t.pnl for t in self.trades)

    @property
    def trade_count(self) -> int:
        return len(self.trades)

    @property
    def win_rate(self) -> float:
        if not self.trades:
            return 0.0
        return sum(1 for t in self.trades if t.is_win) / len(self.trades)

    @property
    def max_drawdown(self) -> float:
        equity = 0.0
        peak = 0.0
        max_dd = 0.0
        for t in self.trades:
            equity += t.pnl
            peak = max(peak, equity)
            max_dd = min(max_dd, equity - peak)
        return max_dd

    @property
    def avg_trade_pnl(self) -> float:
        if not self.trades:
            return 0.0
        return self.total_pnl / len(self.trades)


def generate_synthetic_data(
    n_bars: int = 200,
    start_price: float = 100.0,
    volatility: float = 0.02,
    drift: float = 0.0002,
    seed: int = 42,
) -> list[Bar]:
    rng = random.Random(seed)
    bars: list[Bar] = []
    price = start_price
    for i in range(n_bars):
        ret = drift + volatility * rng.gauss(0, 1)
        open_price = price
        close_price = price * (1 + ret)
        intra_vol = abs(rng.gauss(0, volatility * 0.5))
        high_price = max(open_price, close_price) * (1 + intra_vol)
        low_price = min(open_price, close_price) * (1 - intra_vol)
        volume = rng.uniform(500_000, 2_000_000) * (1 + abs(ret) * 20)
        bars.append(Bar(
            timestamp=i,
            open=round(open_price, 4),
            high=round(high_price, 4),
            low=round(low_price, 4),
            close=round(close_price, 4),
            volume=round(volume, 2),
        ))
        price = close_price
    return bars


def sma(values: list[float], period: int) -> list[float | None]:
    result: list[float | None] = []
    for i in range(len(values)):
        if i < period - 1:
            result.append(None)
        else:
            result.append(sum(values[i - period + 1 : i + 1]) / period)
    return result


def ema(values: list[float], period: int) -> list[float | None]:
    result: list[float | None] = []
    k = 2.0 / (period + 1)
    for i in range(len(values)):
        if i < period - 1:
            result.append(None)
        elif i == period - 1:
            result.append(sum(values[:period]) / period)
        else:
            prev = result[-1]
            assert prev is not None
            result.append(values[i] * k + prev * (1 - k))
    return result


def rsi(closes: list[float], period: int = 14) -> list[float | None]:
    result: list[float | None] = [None] * period
    gains = []
    losses = []
    for i in range(1, len(closes)):
        delta = closes[i] - closes[i - 1]
        gains.append(max(delta, 0))
        losses.append(max(-delta, 0))
    if len(gains) < period:
        return [None] * len(closes)
    avg_gain = sum(gains[:period]) / period
    avg_loss = sum(losses[:period]) / period
    if avg_loss == 0:
        result.append(100.0)
    else:
        rs = avg_gain / avg_loss
        result.append(100 - 100 / (1 + rs))
    for i in range(period, len(gains)):
        avg_gain = (avg_gain * (period - 1) + gains[i]) / period
        avg_loss = (avg_loss * (period - 1) + losses[i]) / period
        if avg_loss == 0:
            result.append(100.0)
        else:
            rs = avg_gain / avg_loss
            result.append(100 - 100 / (1 + rs))
    return result


def bollinger_bands(
    closes: list[float], period: int = 20, num_std: float = 2.0
) -> tuple[list[float | None], list[float | None], list[float | None]]:
    middle = sma(closes, period)
    upper: list[float | None] = []
    lower: list[float | None] = []
    for i in range(len(closes)):
        if middle[i] is None:
            upper.append(None)
            lower.append(None)
        else:
            window = closes[i - period + 1 : i + 1]
            std = (sum((x - middle[i]) ** 2 for x in window) / period) ** 0.5
            upper.append(middle[i] + num_std * std)
            lower.append(middle[i] - num_std * std)
    return upper, middle, lower


def atr(bars: list[Bar], period: int = 14) -> list[float | None]:
    result: list[float | None] = [None]
    for i in range(1, len(bars)):
        tr = max(
            bars[i].high - bars[i].low,
            abs(bars[i].high - bars[i - 1].close),
            abs(bars[i].low - bars[i - 1].close),
        )
        if i < period:
            result.append(None)
        elif i == period:
            trs = []
            for j in range(1, period + 1):
                t = max(
                    bars[j].high - bars[j].low,
                    abs(bars[j].high - bars[j - 1].close),
                    abs(bars[j].low - bars[j - 1].close),
                )
                trs.append(t)
            result.append(sum(trs) / period)
        else:
            prev = result[-1]
            assert prev is not None
            result.append((prev * (period - 1) + tr) / period)
    return result


class SimpleMomentumStrategy:
    def __init__(self, fast: int = 5, slow: int = 20, vol_mult: float = 1.2):
        self.fast = fast
        self.slow = slow
        self.vol_mult = vol_mult

    def generate_signals(self, bars: list[Bar]) -> list[str]:
        closes = [b.close for b in bars]
        volumes = [b.volume for b in bars]
        ma_fast = sma(closes, self.fast)
        ma_slow = sma(closes, self.slow)
        vol_ma = sma(volumes, 20)
        signals: list[str] = []
        for i in range(len(bars)):
            if ma_fast[i] is None or ma_slow[i] is None or vol_ma[i] is None:
                signals.append("Hold")
                continue
            vol_confirm = volumes[i] > vol_ma[i] * self.vol_mult
            if ma_fast[i] > ma_slow[i] and vol_confirm:
                signals.append("Buy")
            elif ma_fast[i] < ma_slow[i] and vol_confirm:
                signals.append("Sell")
            else:
                signals.append("Hold")
        return signals


class MeanReversionStrategy:
    def __init__(
        self,
        bb_period: int = 20,
        bb_std: float = 2.0,
        rsi_period: int = 14,
        rsi_oversold: float = 30.0,
        rsi_overbought: float = 70.0,
    ):
        self.bb_period = bb_period
        self.bb_std = bb_std
        self.rsi_period = rsi_period
        self.rsi_oversold = rsi_oversold
        self.rsi_overbought = rsi_overbought

    def generate_signals(self, bars: list[Bar]) -> list[str]:
        closes = [b.close for b in bars]
        upper, middle, lower = bollinger_bands(closes, self.bb_period, self.bb_std)
        rsi_vals = rsi(closes, self.rsi_period)
        signals: list[str] = []
        for i in range(len(bars)):
            if upper[i] is None or lower[i] is None or rsi_vals[i] is None:
                signals.append("Hold")
                continue
            if closes[i] <= lower[i] and rsi_vals[i] < self.rsi_oversold:
                signals.append("Buy")
            elif closes[i] >= upper[i] and rsi_vals[i] > self.rsi_overbought:
                signals.append("Sell")
            else:
                signals.append("Hold")
        return signals


class TrendFollowingStrategy:
    def __init__(
        self,
        breakout_period: int = 20,
        atr_period: int = 14,
        atr_multiplier: float = 2.0,
    ):
        self.breakout_period = breakout_period
        self.atr_period = atr_period
        self.atr_multiplier = atr_multiplier

    def generate_signals(self, bars: list[Bar]) -> list[str]:
        closes = [b.close for b in bars]
        ma = sma(closes, self.breakout_period)
        atr_vals = atr(bars, self.atr_period)
        signals: list[str] = []
        for i in range(len(bars)):
            if ma[i] is None or atr_vals[i] is None:
                signals.append("Hold")
                continue
            if closes[i] > ma[i] + self.atr_multiplier * atr_vals[i]:
                signals.append("Buy")
            elif closes[i] < ma[i] - self.atr_multiplier * atr_vals[i]:
                signals.append("Sell")
            else:
                signals.append("Hold")
        return signals


def run_backtest(
    bars: list[Bar], signals: list[str], quantity: float = 100.0
) -> BacktestResult:
    result = BacktestResult()
    position: float = 0.0
    entry_price: float = 0.0
    entry_bar: int = 0
    for i in range(len(bars)):
        sig = signals[i]
        price = bars[i].close
        if sig == "Buy" and position == 0:
            position = quantity
            entry_price = price
            entry_bar = i
        elif sig == "Sell" and position > 0:
            result.trades.append(Trade(
                entry_price=entry_price,
                exit_price=price,
                side="Buy",
                quantity=quantity,
                entry_bar=entry_bar,
                exit_bar=i,
            ))
            position = 0.0
    if position > 0:
        result.trades.append(Trade(
            entry_price=entry_price,
            exit_price=bars[-1].close,
            side="Buy",
            quantity=quantity,
            entry_bar=entry_bar,
            exit_bar=len(bars) - 1,
        ))
    return result


def print_backtest_summary(result: BacktestResult, name: str) -> None:
    separator()
    value("策略名称", name)
    value("总交易次数", result.trade_count)
    value("总盈亏", pnl_color(result.total_pnl))
    value("胜率", f"{result.win_rate:.1%}")
    value("平均单笔盈亏", pnl_color(result.avg_trade_pnl))
    value("最大回撤", pnl_color(result.max_drawdown))
    if result.trades:
        wins = [t for t in result.trades if t.is_win]
        losses = [t for t in result.trades if not t.is_win]
        if wins:
            value("最大单笔盈利", pnl_color(max(t.pnl for t in wins)))
        if losses:
            value("最大单笔亏损", pnl_color(min(t.pnl for t in losses)))
        value("盈利笔数", f"{GREEN}{len(wins)}{RESET}")
        value("亏损笔数", f"{RED}{len(losses)}{RESET}")
    separator()


def print_signal_distribution(signals: list[str], name: str) -> None:
    buy_count = signals.count("Buy")
    sell_count = signals.count("Sell")
    hold_count = signals.count("Hold")
    total = len(signals)
    info(f"[{name}] 信号分布 — Buy: {buy_count} ({buy_count/total:.1%}) | "
         f"Sell: {sell_count} ({sell_count/total:.1%}) | "
         f"Hold: {hold_count} ({hold_count/total:.1%})")


def main() -> int:
    header("AXON Quant 传统量化策略演示", "📈")

    step(1, "生成合成市场数据（200 根 K 线）")
    bars = generate_synthetic_data(n_bars=200, start_price=100.0, seed=42)
    value("K 线数量", len(bars))
    value("起始价格", f"{bars[0].close:.2f}")
    value("结束价格", f"{bars[-1].close:.2f}")
    price_change = (bars[-1].close - bars[0].close) / bars[0].close
    value("价格变动", f"{price_change:+.2%}")
    ok("合成数据生成完成")

    step(2, "SimpleMomentumStrategy —— 双均线交叉 + 成交量确认")
    momentum = SimpleMomentumStrategy(fast=5, slow=20, vol_mult=1.2)
    mom_signals = momentum.generate_signals(bars)
    print_signal_distribution(mom_signals, "动量策略")
    mom_result = run_backtest(bars, mom_signals)
    print_backtest_summary(mom_result, "SimpleMomentumStrategy (MA5/MA20 + Vol)")
    ok("动量策略回测完成")

    step(3, "MeanReversionStrategy —— 布林带 + RSI 均值回归")
    mean_rev = MeanReversionStrategy(
        bb_period=20, bb_std=2.0, rsi_period=14,
        rsi_oversold=30.0, rsi_overbought=70.0,
    )
    mr_signals = mean_rev.generate_signals(bars)
    print_signal_distribution(mr_signals, "均值回归策略")
    mr_result = run_backtest(bars, mr_signals)
    print_backtest_summary(mr_result, "MeanReversionStrategy (BB20 + RSI14)")
    ok("均值回归策略回测完成")

    step(4, "TrendFollowingStrategy —— 均线突破 + ATR 止损")
    trend = TrendFollowingStrategy(
        breakout_period=20, atr_period=14, atr_multiplier=2.0,
    )
    trend_signals = trend.generate_signals(bars)
    print_signal_distribution(trend_signals, "趋势跟踪策略")
    trend_result = run_backtest(bars, trend_signals)
    print_backtest_summary(trend_result, "TrendFollowingStrategy (MA20 Breakout + ATR)")
    ok("趋势跟踪策略回测完成")

    step(5, "策略横向对比")
    separator()
    strategies = [
        ("SimpleMomentum", mom_result),
        ("MeanReversion", mr_result),
        ("TrendFollowing", trend_result),
    ]
    print(f"    {BOLD}{'策略':<20} {'交易次数':>8} {'总盈亏':>12} {'胜率':>8} {'最大回撤':>12}{RESET}")
    separator()
    for name, res in strategies:
        pnl_str = f"{res.total_pnl:>+10.2f}"
        dd_str = f"{res.max_drawdown:>10.2f}"
        color = GREEN if res.total_pnl > 0 else RED
        print(f"    {name:<20} {res.trade_count:>8} {color}{pnl_str}{RESET} {res.win_rate:>7.1%} {color}{dd_str}{RESET}")
    separator()

    best = max(strategies, key=lambda x: x[1].total_pnl)
    ok(f"最佳策略: {best[0]}（总盈亏: {pnl_color(best[1].total_pnl)}）")

    ok("传统量化策略演示完成！覆盖动量 + 均值回归 + 趋势跟踪\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
