#!/usr/bin/env python3
"""AXON Quant 风控引擎演示 —— 面向初学者的完整功能展示。

覆盖:
  1. DefaultRiskEngine 初始化与配置
  2. 正常订单通过风控检查
  3. 超额订单被拒绝
  4. 日内亏损更新与熔断触发
  5. 独立 CircuitBreaker 演示
  6. 风险指标快照

运行方式:
    source .venv/bin/activate
    python examples/10_risk/risk_demo.py

零外部依赖: 仅使用 axon_quant + Python 标准库。
"""

from __future__ import annotations

import sys
from typing import Any

# ─── ANSI 颜色 ──────────────────────────────────────────────────────────
RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
RED = "\033[31m"
GREEN = "\033[32m"
YELLOW = "\033[33m"
CYAN = "\033[36m"

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


def fail(msg: str) -> None:
    print(f"    {RED}❌ {msg}{RESET}")


def value(label: str, v: Any, width: int = 20) -> None:
    print(f"    {BOLD}{label:<{width}}{RESET} {v}")


def separator() -> None:
    print(f"    {DIM}{'─' * 50}{RESET}")


def main() -> int:
    from axon_quant.risk import (
        DefaultRiskEngine,
        CircuitBreaker,
        make_order,
        make_portfolio,
        make_risk_config,
    )

    header("AXON Quant 风控引擎演示", "🛡️")

    # ── 1. 创建风控引擎 ──────────────────────────────────────────────────
    step(1, "创建 DefaultRiskEngine（自定义风控参数）")
    config = make_risk_config(
        max_position_per_instrument=100_000.0,
        max_total_exposure=1_000_000.0,
        max_order_value=50_000.0,
        max_leverage=5.0,
        max_drawdown=0.15,
        max_daily_loss=10_000.0,
        max_concentration=0.40,
        circuit_breaker_cooldown_secs=3600,
    )
    engine = DefaultRiskEngine(config)
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 500_000.0})
    value("单笔最大订单价值", "50,000 USDT")
    value("日内最大亏损", "10,000 USDT")
    value("最大杠杆", "5.0x")
    value("最大回撤", "15%")
    value("初始现金", "500,000 USDT")
    ok("风控引擎初始化完成")

    # ── 2. 正常订单通过 ──────────────────────────────────────────────────
    step(2, "提交正常订单 —— 应该通过风控检查")
    normal_order = make_order(
        id=1, symbol="BTC-USDT", side="Buy", type="limit",
        quantity=0.5, price=50_000.0,
    )
    result = engine.check_order(normal_order, portfolio)
    value("订单", "Buy 0.5 BTC @ 50,000")
    value("订单价值", "25,000 USDT")
    value("是否允许", result.is_allow)
    if result.is_allow:
        ok("订单通过风控检查")
    else:
        fail(f"订单被拒: {result.reason}")

    # ── 3. 超额订单被拒 ──────────────────────────────────────────────────
    step(3, "提交超额订单 —— 应该被风控拒绝")
    huge_order = make_order(
        id=2, symbol="BTC-USDT", side="Buy", type="limit",
        quantity=2.0, price=50_000.0,
    )
    result2 = engine.check_order(huge_order, portfolio)
    value("订单", "Buy 2.0 BTC @ 50,000")
    value("订单价值", "100,000 USDT")
    value("是否允许", result2.is_allow)
    if not result2.is_allow:
        ok(f"订单被拒 — 原因: {result2.reason}")
    else:
        warn("订单意外通过")

    # ── 4. 日内亏损触发熔断 ──────────────────────────────────────────────
    step(4, "模拟日内亏损，触发熔断器")
    engine.update_daily_pnl(-8_000.0)
    info("当前日内亏损: -8,000 USDT（阈值 10,000 USDT）")
    result3 = engine.check_order(normal_order, portfolio)
    value("是否允许", result3.is_allow)
    if not result3.is_allow:
        ok(f"熔断器触发！订单被拒 — 原因: {result3.reason}")
    else:
        warn("熔断器未触发（可能阈值未达到）")

    engine.update_daily_pnl(-5_000.0)
    info("累计日内亏损: -13,000 USDT（超过 10,000 阈值）")
    result4 = engine.check_order(normal_order, portfolio)
    value("是否允许", result4.is_allow)
    if not result4.is_allow:
        ok(f"熔断器确认触发 — 原因: {result4.reason}")
    else:
        warn("熔断器未触发")

    # ── 5. 独立 CircuitBreaker 演示 ─────────────────────────────────────
    step(5, "独立 CircuitBreaker —— 单次大额亏损触发")
    cb = CircuitBreaker(daily_loss_limit=5_000.0, cooldown_seconds=3600)
    value("亏损阈值", "5,000 USDT")
    value("冷却时间", "3600 秒")

    cb.check_and_trigger(-2_000.0)
    status = "🔴 触发" if cb.is_active else "🟢 正常"
    value("单次亏损 -2,000", f"状态: {status}")

    cb.check_and_trigger(-6_000.0)
    status = "🔴 触发" if cb.is_active else "🟢 正常"
    value("单次亏损 -6,000", f"状态: {status}")

    if cb.is_active:
        ok("熔断器因单笔亏损超过阈值而触发")
    else:
        warn("熔断器未触发")

    # ── 6. 风险指标快照 ──────────────────────────────────────────────────
    step(6, "风险指标快照")
    engine.reset_daily()
    info("已重置日内亏损")
    metrics = engine.metrics(portfolio)
    separator()
    for key, val in metrics.items():
        if isinstance(val, float):
            value(key, f"{val:,.4f}")
        else:
            value(key, val)
    separator()
    ok("风险指标包含: 暴露度、杠杆率、集中度、回撤、VaR 等")

    separator()
    ok("风控引擎演示完成！覆盖预交易检查 + 熔断器 + 风险指标\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
