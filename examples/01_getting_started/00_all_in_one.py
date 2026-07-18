#!/usr/bin/env python3
"""AXON Quant 一站式交互式教程 —— 面向初学者的完整功能演示。

覆盖全部 6 个 Stage:
  1. 数据服务 (Data)      — MockSource + DataService + Arrow 零拷贝
  2. 回测引擎 (Backtest)   — L1 撮合 + 事件驱动回测
  3. 风控系统 (Risk)       — 预交易检查 + 熔断器 + 风险指标
  4. 订单管理 (OMS)        — 订单状态机 + Portfolio 管理
  5. 交易工具 (Trading)    — Mock 后端 + 下单/撤单/改单
  6. RL 强化学习环境       — TradingEnv + 随机策略

运行方式:
    source .venv/bin/activate
    python examples/01_getting_started/00_all_in_one.py

交互方式:
    启动后选择编号进入对应模块体验，输入 0 退出。
    若安装了 typer 则提供增强 CLI，否则使用内置 input()。

零外部依赖: 所有数据均为内置离线数据，无需网络。
"""

from __future__ import annotations

import sys
import time
import textwrap
from typing import Any

# ─── 增强 CLI: 尝试 import typer，未安装则优雅降级 ─────────────────────
try:
    import typer

    HAS_TYPER = True
except ImportError:
    HAS_TYPER = False

# ─── 彩色输出工具 ──────────────────────────────────────────────────────
# 跨平台 ANSI 颜色，Windows 终端也支持（Python 3.10+ 自动启用 VT100）
RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
RED = "\033[31m"
GREEN = "\033[32m"
YELLOW = "\033[33m"
BLUE = "\033[34m"
MAGENTA = "\033[35m"
CYAN = "\033[36m"

# Windows 终端启用 ANSI
if sys.platform == "win32":
    try:
        import os
        os.system("")  # 启用 VT100
    except Exception:
        pass


def header(title: str, icon: str = "▶") -> None:
    """打印彩色章节标题。"""
    print(f"\n{BOLD}{CYAN}{'═' * 60}{RESET}")
    print(f"{BOLD}{CYAN}  {icon} {title}{RESET}")
    print(f"{BOLD}{CYAN}{'═' * 60}{RESET}")


def step(n: int, text: str) -> None:
    """打印步骤编号。"""
    print(f"\n  {BOLD}{YELLOW}[步骤 {n}]{RESET} {text}")


def ok(msg: str) -> None:
    """打印成功消息。"""
    print(f"    {GREEN}✅ {msg}{RESET}")


def info(msg: str) -> None:
    """打印信息。"""
    print(f"    {DIM}{msg}{RESET}")


def warn(msg: str) -> None:
    """打印警告。"""
    print(f"    {YELLOW}⚠️  {msg}{RESET}")


def fail(msg: str) -> None:
    """打印失败。"""
    print(f"    {RED}❌ {msg}{RESET}")


def value(label: str, v: Any, width: int = 20) -> None:
    """打印一个 label: value 对。"""
    print(f"    {BOLD}{label:<{width}}{RESET} {v}")


def separator() -> None:
    print(f"    {DIM}{'─' * 50}{RESET}")


# ══════════════════════════════════════════════════════════════════════════
# Stage 1: 数据服务
# ══════════════════════════════════════════════════════════════════════════


def demo_data() -> None:
    """Stage 1: 数据服务 —— MockSource + DataService + Arrow 零拷贝。"""
    header("Stage 1: 数据服务 (Data)", "📦")

    # 1. 创建 MockSource 生成合成 tick 数据
    step(1, "创建 MockSource，生成 1000 条合成 BTC tick 数据")
    from axon_quant.data import DataService, MockSource, Frequency, DataRequest

    svc = (
        DataService.new()
        .register_source(
            MockSource.with_tick_series(
                "btc",          # 数据集名
                1000,           # tick 数量
                1_000_000,      # 初始序列号
                lambda i: 100.0 + i * 0.01,  # 价格函数: 100 + i*0.01
            )
        )
        .with_cache_capacity(64)
    )
    ok("MockSource 注册完成，数据集: btc (1000 条 tick)")

    # 2. 构造 DataRequest 并加载数据
    step(2, "构造 DataRequest 并通过 DataService 加载")
    req = DataRequest(
        symbol="BTCUSDT",
        start="2026-01-01T00:00:00Z",
        end="2026-01-02T00:00:00Z",
        frequency=Frequency.Tick,
    )
    ds = svc.load(req)
    value("数据长度", f"{ds.len} 条")
    value("校验和前 8 字节", ds.checksum[:8])

    # 3. Arrow 零拷贝读取
    step(3, "通过 Arrow RecordBatch 零拷贝读取数据")
    batch = ds.to_arrow(0)
    value("RecordBatch 行数", batch.num_rows)
    value("RecordBatch 列数", batch.num_columns)
    value("Schema", [f.name for f in batch.schema])
    # 打印前 3 行
    print(f"\n    {DIM}前 3 行数据:{RESET}")
    for i in range(min(3, batch.num_rows)):
        row = {col: batch.column(col)[i].as_py() for col in batch.schema.names}
        print(f"    {DIM}  {row}{RESET}")

    # 4. 缓存统计
    step(4, "查看缓存统计")
    stats = svc.cache_stats()
    value("缓存命中次数", stats.hits)
    value("缓存未命中次数", stats.misses)
    value("缓存容量", stats.capacity)
    value("命中率", f"{stats.hit_rate:.3f}")

    # 再次加载相同请求，触发缓存命中
    svc.load(req)
    stats2 = svc.cache_stats()
    value("再次加载后命中次数", stats2.hits)
    ok("缓存机制生效：第二次加载直接命中缓存")

    separator()
    ok("Stage 1 完成！数据服务支持 MockSource/Arrow 零拷贝/缓存\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 2: 回测引擎
# ══════════════════════════════════════════════════════════════════════════


def demo_backtest() -> None:
    """Stage 2: 回测引擎 —— L1 撮合 + 事件驱动回测。"""
    header("Stage 2: 回测引擎 (Backtest)", "📈")

    from axon_quant.backtest import (
        L1MatchingEngine,
        BacktestEngine,
        L2MatchingEngine,
        limit_order,
        market_order,
        spot_instrument,
    )

    # 0.6.0 起:`symbol` 字符串改 `instrument` 字典(spot / swap 区分)
    btc_spot = spot_instrument("BTC", "USDT")
    eth_spot = spot_instrument("ETH", "USDT")

    # 1. L1 撮合引擎：限价单撮合
    step(1, "L1 撮合引擎 —— 限价单匹配")
    engine = L1MatchingEngine()

    # 先挂一个卖单
    sell_result = engine.submit(limit_order(1, btc_spot, "Sell", 100.0, 1.0))
    value("卖单 ID", 1)
    value("是否成交", sell_result["is_filled"])
    info("卖单挂单成功，等待买单匹配")

    # 提交买单，价格匹配
    buy_result = engine.submit(limit_order(2, btc_spot, "Buy", 100.0, 1.0))
    value("买单 ID", 2)
    value("是否成交", buy_result["is_filled"])
    value("成交笔数", len(buy_result["fills"]))
    ok("限价单撮合成功：买卖双方在 100.00 成交 1.0 BTC")

    # 2. L1 撮合引擎：市价单
    step(2, "L1 撮合引擎 —— 市价单吃单")
    engine2 = L1MatchingEngine()
    engine2.submit(limit_order(10, eth_spot, "Sell", 2000.0, 5.0))
    mkt_result = engine2.submit(market_order(11, eth_spot, "Buy", 2.0))
    value("市价买单是否成交", mkt_result["is_filled"])
    if mkt_result["fills"]:
        fill = mkt_result["fills"][0]
        value("成交价格", fill["price"])
        value("成交数量", fill["quantity"])
    ok("市价单以对手盘最优价即时成交")

    # 3. 事件驱动回测
    step(3, "BacktestEngine —— 事件驱动回测")
    bt = BacktestEngine(initial_cash=100_000.0)
    # 推入多个事件模拟交易
    events = [
        {
            "type": "order_submitted",
            "timestamp_ns": 1_000_000_000,
            "order": limit_order(1, btc_spot, "Buy", 50_000.0, 0.1),
        },
        {
            "type": "order_submitted",
            "timestamp_ns": 2_000_000_000,
            "order": limit_order(2, btc_spot, "Buy", 50_000.0, 0.1),
        },
    ]
    for evt in events:
        bt.push_event(evt)

    result = bt.run()
    value("最终 NAV", f"{result.final_nav:,.2f}")
    value("初始资金", "100,000.00")
    ok("事件驱动回测完成")

    separator()
    ok("Stage 2 完成！支持 L1/L2 撮合 + 事件驱动回测\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 3: 风控系统
# ══════════════════════════════════════════════════════════════════════════


def demo_risk() -> None:
    """Stage 3: 风控系统 —— 预交易检查 + 熔断器 + 风险指标。"""
    header("Stage 3: 风控系统 (Risk)", "🛡️")

    from axon_quant.risk import (
        DefaultRiskEngine,
        CircuitBreaker,
        make_order,
        make_portfolio,
        make_risk_config,
    )

    # 1. 创建风控引擎
    step(1, "创建风控引擎（单笔最大 50,000 USDT，日内最大亏损 10,000 USDT）")
    config = make_risk_config(
        max_order_value=50_000.0,
        max_daily_loss=10_000.0,
        max_position_per_instrument=200_000.0,
        max_leverage=3.0,
    )
    engine = DefaultRiskEngine(config)
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    ok("风控引擎就绪")

    # 2. 正常订单通过
    step(2, "提交正常订单 —— 应该通过")
    normal_order = make_order(
        id=1, symbol="BTC-USDT", side="Buy", type="limit",
        price=50_000.0, quantity=0.5,
    )
    result = engine.check_order(normal_order, portfolio)
    value("是否允许", result.is_allow)
    if result.is_allow:
        ok("订单通过风控检查 ✅")
    else:
        warn(f"订单被拒: {result.reason}")

    # 3. 超大订单被拒
    step(3, "提交超限订单 —— 应该被拒")
    huge_order = make_order(
        id=2, symbol="BTC-USDT", side="Buy", type="limit",
        price=50_000.0, quantity=2.0,  # 价值 100,000 > 50,000 限制
    )
    result2 = engine.check_order(huge_order, portfolio)
    value("是否允许", result2.is_allow)
    if not result2.is_allow:
        ok(f"订单被拒 — 原因: {result2.reason}")
    else:
        warn("订单意外通过")

    # 4. 日内亏损触发熔断
    step(4, "日内亏损触发熔断")
    engine.update_daily_pnl(-8_000.0)
    info("模拟日内亏损: -8,000 USDT（接近 10,000 阈值）")

    result3 = engine.check_order(normal_order, portfolio)
    value("是否允许", result3.is_allow)
    if not result3.is_allow:
        ok(f"熔断器触发！订单被拒 — 原因: {result3.reason}")
    else:
        warn("熔断器未触发")

    # 5. 独立熔断器演示
    step(5, "独立熔断器 —— 单次大额亏损触发")
    cb = CircuitBreaker(daily_loss_limit=5_000.0, cooldown_seconds=3600)
    info("熔断器阈值: 单次亏损 5,000 USDT, 冷却 3600 秒")
    cb.check_and_trigger(-2_000.0)
    value("单次亏损 2,000", f"熔断器状态: {'🔴 触发' if cb.is_active else '🟢 正常'}")
    cb.check_and_trigger(-6_000.0)
    value("单次亏损 6,000", f"熔断器状态: {'🔴 触发' if cb.is_active else '🟢 正常'}")

    # 6. 风险指标
    step(6, "风险指标快照")
    engine.reset_daily()  # 重置日内亏损
    metrics = engine.metrics(portfolio)
    for key, val in metrics.items():
        if isinstance(val, float):
            value(key, f"{val:,.4f}")
        else:
            value(key, val)

    separator()
    ok("Stage 3 完成！风控支持预交易检查 + 熔断器 + VaR 等指标\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 4: 订单管理
# ══════════════════════════════════════════════════════════════════════════


def demo_oms() -> None:
    """Stage 4: 订单管理 (OMS) —— 订单状态机 + Portfolio 管理。"""
    header("Stage 4: 订单管理 (OMS)", "📋")

    from axon_quant.oms import (
        OrderManager,
        OrderStatus,
        Side,
        OrderType,
        make_order_status,
    )
    from axon_quant.oms import limit_order, market_order

    # 1. 创建 OMS 并入金
    step(1, "创建 OrderManager 并入金 100,000 USDT")
    oms = OrderManager()
    oms.deposit("USDT", 100_000.0)
    snap = oms.snapshot_balance()
    value("当前 USDT 余额", f"{float(snap['cash'].get('USDT', 0)):,.2f}")
    ok("入金成功")

    # 2. 提交限价买单
    step(2, "提交限价买单: 买入 0.1 BTC @ 50,000")
    oid = oms.submit(
        limit_order("BTC-USDT", "Buy", 0.1, 50_000, idempotency_key="order-001")
    )
    value("订单 ID", oid)
    status = oms.get_order_status(oid)
    value("当前状态", status.kind)
    ok("订单提交成功，状态自动流转为 Submitted")

    # 3. 模拟交易所确认
    step(3, "模拟交易所确认 (Submitted → Acknowledged)")
    oms.update_status(oid, make_order_status("Acknowledged"))
    ok("交易所确认: Acknowledged")

    # 4. 部分成交
    step(4, "模拟部分成交 (Acknowledged → PartiallyFilled)")
    oms.add_fill(
        order_id=oid,
        fill_id="fill-001",
        symbol="BTC-USDT",
        price=50_000.0,
        quantity=0.05,  # 买入 0.05 BTC (正数 = buy)
        fee=0.25,
    )
    info("成交 0.05 BTC @ 50,000, 手续费 0.25 USDT")
    status_mid = oms.get_order_status(oid)
    value("当前状态", status_mid.kind)

    # 5. 完全成交
    step(5, "模拟完全成交 (PartiallyFilled → Filled)")
    oms.add_fill(
        order_id=oid,
        fill_id="fill-002",
        symbol="BTC-USDT",
        price=50_000.0,
        quantity=0.05,  # 再成交 0.05 BTC
        fee=0.25,
    )
    status_final = oms.get_order_status(oid)
    value("最终状态", status_final.kind)
    ok("订单完全成交: Filled")

    # 6. Portfolio 快照
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
    info("余额减少 = 买入 0.1 BTC × 50,000 + 手续费 0.50 = 5,000.50 USDT")

    # 7. 批量提交
    step(7, "批量提交多个订单")
    orders = [
        limit_order("ETH-USDT", "Buy", 1.0, 3_000, idempotency_key=f"batch-{i}")
        for i in range(3)
    ]
    ids = oms.batch_submit(orders)
    value("批量提交订单数", len(ids))
    value("活跃订单数", oms.active_count())
    ok("批量提交成功")

    separator()
    ok("Stage 4 完成！OMS 支持订单状态机 + Portfolio + 批量操作\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 5: 交易工具
# ══════════════════════════════════════════════════════════════════════════


def demo_trading() -> None:
    """Stage 5: 交易工具 —— Mock 后端 + 下单/撤单/改单。"""
    header("Stage 5: 交易工具 (Trading)", "🔄")

    from axon_quant.trading import (
        RiskLimits,
        MockTradingBackend,
        PlaceOrderTool,
        QueryPortfolioTool,
        CancelOrderTool,
        ReplaceOrderTool,
        TradingMetrics,
    )

    # 1. 初始化 Mock 后端
    step(1, "初始化 MockTradingBackend + 风控限制")
    backend = MockTradingBackend()
    risk = RiskLimits(
        max_order_notional=100_000.0,
        max_daily_orders=50,
        allowed_symbols=["BTC-USDT", "ETH-USDT"],
    )
    ok("Mock 后端就绪（模拟交易所，不连接真实网络）")

    # 2. Dry Run 下单
    step(2, "Dry Run 模式下单 —— 仅验证，不实际执行")
    place = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk)
    ack = place.execute({
        "symbol": "BTC-USDT",
        "side": "Buy",
        "quantity": 0.5,
        "price": 50_000.0,
    })
    value("下单状态", ack.get("status", "unknown"))
    value("订单 ID", ack.get("order_id", "N/A"))
    ok("Dry Run 完成 — 订单未实际发送")

    # 3. Direct 模式下单
    step(3, "Direct 模式下单 —— 实际执行到 Mock 后端")
    place_direct = PlaceOrderTool(backend=backend, mode="direct", risk=risk)
    ack2 = place_direct.execute({
        "symbol": "BTC-USDT",
        "side": "Buy",
        "quantity": 0.1,
        "price": 50_000.0,
    })
    value("下单状态", ack2.get("status", "unknown"))
    order_id = ack2.get("order_id", "MOCK-1")
    value("订单 ID", order_id)
    ok("Mock 后端接收订单")

    # 4. 查询组合
    step(4, "查询当前组合")
    query = QueryPortfolioTool(backend=backend)
    portfolio = query.execute()
    value("组合状态", portfolio)

    # 5. 撤单
    step(5, "撤销订单")
    cancel = CancelOrderTool(backend=backend, risk=risk)
    cancel_result = cancel.execute({"order_id": order_id})
    value("撤单结果", cancel_result)
    ok(f"订单 {order_id} 已撤销")

    # 6. 改单
    step(6, "改单 —— 修改价格和数量")
    # 先下一个新单
    ack3 = place_direct.execute({
        "symbol": "ETH-USDT",
        "side": "Sell",
        "quantity": 2.0,
        "price": 3_000.0,
    })
    oid_replace = ack3.get("order_id", "MOCK-2")
    info(f"原订单: 卖出 2.0 ETH @ 3,000 (ID: {oid_replace})")

    replace = ReplaceOrderTool(backend=backend, risk=risk)
    replace_result = replace.execute({
        "order_id": oid_replace,
        "new_req": {
            "symbol": "ETH-USDT",
            "side": "Sell",
            "quantity": 3.0,
            "price": 3_100.0,
        },
    })
    value("改单结果", replace_result)
    ok("改单成功: 数量 2.0→3.0, 价格 3,000→3,100")

    # 7. 指标统计
    step(7, "TradingMetrics 指标埋点")
    metrics = TradingMetrics()
    place_m = PlaceOrderTool(backend=backend, mode="direct", risk=risk, metrics=metrics)
    # 连续执行 3 次下单
    for i in range(3):
        place_m.execute({
            "symbol": "BTC-USDT",
            "side": "Buy",
            "quantity": 0.01 * (i + 1),
            "price": 50_000.0 + i * 100,
        })
    samples = metrics.snapshot()
    value("指标采样数", len(samples))
    if samples:
        for s in samples[:5]:
            info(f"  {s}")

    separator()
    ok("Stage 5 完成！Trading 支持 Mock 后端 + 下单/撤单/改单 + 指标\n")


# ══════════════════════════════════════════════════════════════════════════
# Stage 6: RL 强化学习环境
# ══════════════════════════════════════════════════════════════════════════


def demo_rl() -> None:
    """Stage 6: RL 强化学习环境 —— TradingEnv + 随机策略。"""
    header("Stage 6: RL 强化学习环境", "🤖")

    import random as _random
    sys.path.insert(0, str(__import__("pathlib").Path(__file__).resolve().parent.parent))
    from axon_examples import common

    # 1. 生成合成市场数据
    step(1, "生成合成市场数据（几何布朗运动）")
    market_data = common.make_synthetic_market_data(n=500, seed=42)
    value("K 线数量", len(market_data))
    value("起始价格", f"{market_data[0]['close']:.2f}")
    value("结束价格", f"{market_data[-1]['close']:.2f}")
    # 简单统计
    closes = [bar["close"] for bar in market_data]
    min_p, max_p = min(closes), max(closes)
    value("最低价", f"{min_p:.2f}")
    value("最高价", f"{max_p:.2f}")
    value("价格区间", f"{(max_p - min_p) / min_p * 100:.1f}%")
    ok("合成数据就绪 — 无需外部数据文件")

    # 2. 创建 RL 环境
    step(2, "创建 Gymnasium 兼容的 TradingEnv")
    cfg = common.make_env_config(
        initial_capital=100_000.0,
        max_steps=200,
        seed=42,
        symbol="BTCUSDT",
    )
    env = common.make_env(config=cfg, market_data=market_data, reward="pnl")
    value("初始资金", f"{cfg['initial_capital']:,.0f}")
    value("最大步数", cfg["max_steps"])
    value("交易对", cfg["symbol"])
    value("奖励函数", "pnl (盈亏)")
    ok("环境初始化完成 — Gymnasium 标准接口")

    # 3. 环境 reset + step
    step(3, "环境交互: reset → step")
    reset_result = env.reset()
    # env.reset() 返回 dict（含 features / feature_names / timestamp）
    features = reset_result.get("features", [])
    names = reset_result.get("feature_names", [])
    value("观察特征数", len(features))
    value("特征名", names)
    value("初始 timestamp", reset_result.get("timestamp", "N/A"))

    # 执行 3 步
    for i in range(3):
        action = [_random.uniform(-1.0, 1.0)]
        obs_dict, reward, terminated, truncated, step_info = env.step(action)
        pf = step_info.get("portfolio_value", 0) if isinstance(step_info, dict) else 0
        trades = step_info.get("trades_executed", 0) if isinstance(step_info, dict) else 0
        value(f"Step {i+1}", f"action={action[0]:+.3f}, reward={reward:+.4f}, "
              f"portfolio={pf:,.2f}, trades={trades}")
        if terminated or truncated:
            break
    ok("环境交互正常 — 支持标准 Gymnasium reset/step 协议")

    # 4. 完整 episode
    step(4, "运行完整随机策略 episode (200 步)")
    t0 = time.perf_counter()
    result = common.run_random_episode(env, max_steps=200, seed=42)
    elapsed = time.perf_counter() - t0
    value("总步数", result["steps"])
    value("累计奖励", f"{result['total_reward']:.4f}")
    value("最终净值", f"{result['final_value']:,.2f}")
    value("交易次数", result["trades"])
    value("耗时", f"{elapsed:.3f}s")
    ok("随机策略 episode 完成")

    # 5. 多 episode 统计
    step(5, "运行 5 个随机 episode 统计对比")
    records = []
    for i in range(5):
        r = common.run_random_episode(env, max_steps=200, seed=i)
        records.append(r)
        emoji = "🟢" if r["total_reward"] > 0 else "🔴"
        print(f"    {emoji} Episode {i}: reward={r['total_reward']:+.4f}, "
              f"portfolio={r['final_value']:,.2f}, trades={r['trades']}")

    summary = common.summarize(records)
    separator()
    value("平均奖励", f"{summary['mean_reward']:.4f}")
    value("平均最终净值", f"{summary['mean_final_value']:,.2f}")
    value("平均步数", f"{summary['mean_steps']:.0f}")
    ok("随机策略作为基线 — 可对接 PPO/SAC 等 RL 算法进行训练")

    separator()
    ok("Stage 6 完成！RL 环境支持 Gymnasium 协议 + 多种奖励函数\n")


# ══════════════════════════════════════════════════════════════════════════
# 主菜单
# ══════════════════════════════════════════════════════════════════════════

MENU_OPTIONS = {
    "1": ("数据服务 (Data)", demo_data),
    "2": ("回测引擎 (Backtest)", demo_backtest),
    "3": ("风控系统 (Risk)", demo_risk),
    "4": ("订单管理 (OMS)", demo_oms),
    "5": ("交易工具 (Trading)", demo_trading),
    "6": ("RL 强化学习环境", demo_rl),
}


def print_banner() -> None:
    """打印启动横幅。"""
    print(f"""
{BOLD}{CYAN}╔══════════════════════════════════════════════════════════╗
║                                                          ║
║   {BOLD}{MAGENTA}AXON Quant{RESET}{CYAN}  —  一站式交互式教程                    ║
║   {DIM}Rust 核心 + Python 接口 · 量化交易回测框架{RESET}{CYAN}           ║
║                                                          ║
║   {GREEN}全部离线运行，无需网络和外部数据{RESET}{CYAN}                     ║
║                                                          ║
╚══════════════════════════════════════════════════════════╝{RESET}
""")


def print_menu() -> None:
    """打印功能菜单。"""
    print(f"  {BOLD}选择要体验的功能:{RESET}\n")
    for key, (name, _) in sorted(MENU_OPTIONS.items()):
        print(f"    {BOLD}{CYAN}[{key}]{RESET} {name}")
    print(f"    {BOLD}{RED}[0]{RESET} 退出")
    print()


def run_typer() -> None:
    """使用 typer 运行交互式菜单。"""
    app = typer.Typer(
        name="axon-tutorial",
        help="AXON Quant 一站式交互式教程",
        add_completion=False,
    )

    @app.command()
    def main(
        stage: int = typer.Option(
            None, "--stage", "-s",
            help="直接运行指定 Stage (1-6)，不使用交互菜单",
        ),
        all_stages: bool = typer.Option(
            False, "--all", "-a",
            help="依次运行全部 Stage",
        ),
    ) -> None:
        """AXON Quant 一站式交互式教程。

        不带参数启动交互菜单，带 --stage 直接运行指定模块。
        """
        print_banner()

        if all_stages:
            for key in sorted(MENU_OPTIONS.keys()):
                name, func = MENU_OPTIONS[key]
                func()
            print(f"\n  {BOLD}{GREEN}全部 Stage 运行完毕！{RESET}\n")
            return

        if stage is not None:
            key = str(stage)
            if key in MENU_OPTIONS:
                name, func = MENU_OPTIONS[key]
                func()
            else:
                print(f"  {RED}无效的 Stage 编号: {stage}{RESET}")
            return

        # 交互菜单循环
        while True:
            print_menu()
            try:
                choice = input(f"  {BOLD}请输入编号 (0-6): {RESET}").strip()
            except (EOFError, KeyboardInterrupt):
                print(f"\n  {DIM}再见！{RESET}")
                break

            if choice == "0":
                print(f"\n  {BOLD}{GREEN}再见！欢迎再次使用 AXON Quant 🚀{RESET}\n")
                break
            elif choice in MENU_OPTIONS:
                name, func = MENU_OPTIONS[choice]
                try:
                    func()
                except Exception as e:
                    fail(f"执行出错: {e}")
                    import traceback
                    traceback.print_exc()
            else:
                warn(f"无效输入: {choice}，请输入 0-6")

    app()


def run_basic() -> None:
    """使用内置 input() 运行交互式菜单（无 typer 降级模式）。"""
    print_banner()

    while True:
        print_menu()
        try:
            choice = input(f"  {BOLD}请输入编号 (0-6): {RESET}").strip()
        except (EOFError, KeyboardInterrupt):
            print(f"\n  {DIM}再见！{RESET}")
            break

        if choice == "0":
            print(f"\n  {BOLD}{GREEN}再见！欢迎再次使用 AXON Quant 🚀{RESET}\n")
            break
        elif choice in MENU_OPTIONS:
            name, func = MENU_OPTIONS[choice]
            try:
                func()
            except Exception as e:
                fail(f"执行出错: {e}")
                import traceback
                traceback.print_exc()
        else:
            warn(f"无效输入: {choice}，请输入 0-6")


def main() -> int:
    """主入口：根据 typer 可用性选择运行模式。"""
    # 检查命令行参数：--stage / --all 也可以在 basic 模式下支持
    args = sys.argv[1:]

    if "--all" in args or "-a" in args:
        print_banner()
        for key in sorted(MENU_OPTIONS.keys()):
            name, func = MENU_OPTIONS[key]
            func()
        print(f"\n  {BOLD}{GREEN}全部 Stage 运行完毕！{RESET}\n")
        return 0

    # 检查 --stage N
    stage_val = None
    for i, arg in enumerate(args):
        if arg in ("--stage", "-s") and i + 1 < len(args):
            stage_val = args[i + 1]
            break
        if arg.startswith("--stage="):
            stage_val = arg.split("=", 1)[1]
            break

    if stage_val is not None:
        if stage_val in MENU_OPTIONS:
            print_banner()
            name, func = MENU_OPTIONS[stage_val]
            func()
            return 0
        else:
            print(f"  {RED}无效的 Stage 编号: {stage_val}{RESET}")
            return 1

    # 交互模式
    if HAS_TYPER:
        run_typer()
    else:
        run_basic()

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
