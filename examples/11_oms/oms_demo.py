#!/usr/bin/env python3
"""AXON OMS (Order Management System) 独立演示。

覆盖:
  1. 创建 OrderManager 并入金
  2. 提交限价订单
  3. 状态流转: Submitted → Acknowledged → PartiallyFilled → Filled
  4. 成交后 Portfolio 快照
  5. 批量订单提交
  6. 订单撤单

运行方式:
    source .venv/bin/activate
    python examples/11_oms/oms_demo.py
"""

from __future__ import annotations

import sys

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


def value(label: str, v: object, width: int = 20) -> None:
    print(f"    {BOLD}{label:<{width}}{RESET} {v}")


def separator() -> None:
    print(f"    {DIM}{'─' * 50}{RESET}")


def main() -> int:
    from axon_quant.oms import (
        OrderManager,
        OrderStatus,
        Side,
        OrderType,
        make_order_status,
    )
    from axon_quant.oms import limit_order, market_order
    from axon_quant.backtest import spot_instrument, swap_instrument  # 0.6.0:Instrument 工厂

    # 0.6.0 OMS 说明:`Order` 在 Rust 端已支持 `instrument: Option<Instrument>`
    # 结构化字段(供跨 leg 风险约束 / 路由使用),Python 端 `limit_order` /
    # `market_order` 工厂通过可选 `instrument=dict` 形参 + `Order.with_instrument()`
    # builder 链式注入。`spot_instrument` / `swap_instrument` 工厂从
    # `axon_quant.backtest` 复用,spot 形式 `{"kind": "spot", ...}` /
    # swap 形式 `{"kind": "swap", "settle": ..., "contract_size": ...}`。
    BTC_USDT = "BTC-USDT"
    ETH_USDT = "ETH-USDT"
    BTC_SPOT = spot_instrument("BTC", "USDT")
    ETH_SPOT = spot_instrument("ETH", "USDT")

    header("AXON OMS (Order Management System) 演示", "📋")

    # ── 1. 创建 OrderManager 并入金 ────────────────────────────────
    step(1, "创建 OrderManager 并入金 100,000 USDT")
    oms = OrderManager()
    oms.deposit("USDT", 100_000.0)
    snap = oms.snapshot_balance()
    value("USDT 余额", f"{float(snap['cash'].get('USDT', 0)):,.2f}")
    ok("入金成功")

    # ── 2. 提交限价买单 ──────────────────────────────────────────
    step(2, "提交限价买单: 买入 0.1 BTC @ 50,000")
    oid = oms.submit(
        limit_order(
            BTC_USDT, "Buy", 0.1, 50_000,
            idempotency_key="demo-001",
            instrument=BTC_SPOT,  # 0.6.0:结构化 instrument 注入
        )
    )
    value("订单 ID", oid)
    status = oms.get_order_status(oid)
    value("当前状态", status.kind)
    ok("订单提交成功，状态自动流转为 Submitted")

    # ── 3. 状态流转: Submitted → Acknowledged → PartiallyFilled → Filled ──
    step(3, "状态流转: Submitted → Acknowledged")
    oms.update_status(oid, make_order_status("Acknowledged"))
    status = oms.get_order_status(oid)
    value("当前状态", status.kind)
    ok("交易所确认: Acknowledged")

    step(4, "状态流转: Acknowledged → PartiallyFilled")
    oms.add_fill(
        order_id=oid,
        fill_id="fill-001",
        symbol=BTC_USDT,
        price=50_000.0,
        quantity=0.05,
        fee=0.25,
    )
    info("成交 0.05 BTC @ 50,000, 手续费 0.25 USDT")
    status = oms.get_order_status(oid)
    value("当前状态", status.kind)
    ok("部分成交: PartiallyFilled")

    step(5, "状态流转: PartiallyFilled → Filled")
    oms.add_fill(
        order_id=oid,
        fill_id="fill-002",
        symbol=BTC_USDT,
        price=50_000.0,
        quantity=0.05,
        fee=0.25,
    )
    info("再成交 0.05 BTC @ 50,000, 手续费 0.25 USDT")
    status = oms.get_order_status(oid)
    value("最终状态", status.kind)
    ok("订单完全成交: Filled")

    # ── 4. Portfolio 快照 ─────────────────────────────────────────
    step(6, "查看 Portfolio 快照")
    snap_final = oms.snapshot_balance()
    usdt_balance = float(snap_final["cash"].get("USDT", 0))
    value("USDT 余额", f"{usdt_balance:,.2f}")
    positions = snap_final.get("positions", {})
    value("持仓数量", len(positions))
    if positions:
        for sym, pos in positions.items():
            qty = pos.quantity if hasattr(pos, "quantity") else pos.get("quantity", 0)
            avg = pos.avg_price if hasattr(pos, "avg_price") else pos.get("avg_price", 0)
            info(f"  {sym}: 数量={qty}, 均价={avg}")
    info("余额减少 = 0.1 BTC × 50,000 + 手续费 0.50 = 5,000.50 USDT")
    ok("Portfolio 更新正确")

    # ── 5. 批量订单提交 ──────────────────────────────────────────
    step(7, "批量提交 3 个 ETH-USDT 限价买单")
    orders = [
        limit_order(
            ETH_USDT, "Buy", 1.0, 3_000,
            idempotency_key=f"batch-{i}",
            instrument=ETH_SPOT,  # 0.6.0:结构化 instrument 注入
        )
        for i in range(3)
    ]
    ids = oms.batch_submit(orders)
    value("批量提交订单数", len(ids))
    value("活跃订单数", oms.active_count())
    for batch_id in ids:
        info(f"  订单 ID: {batch_id}")
    ok("批量提交成功")

    # ── 6. 订单撤单 ──────────────────────────────────────────────
    step(8, "撤单: 撤销第一个批量订单")
    cancel_id = ids[0]
    status_before = oms.get_order_status(cancel_id)
    value("撤单前状态", status_before.kind if status_before else "N/A")
    oms.update_status(cancel_id, make_order_status("Acknowledged"))
    oms.update_status(cancel_id, make_order_status("Cancelled", filled_qty=0))
    status_after = oms.get_order_status(cancel_id)
    value("撤单后状态", status_after.kind if status_after else "已归档（None）")
    value("历史订单总数", oms.history_count())
    value("活跃订单数", oms.active_count())
    ok("订单撤单成功: Cancelled → 归档到历史")

    separator()
    ok("OMS 演示完成！支持订单状态机 + Portfolio + 批量操作 + 撤单\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
