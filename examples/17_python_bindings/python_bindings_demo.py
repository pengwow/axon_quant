#!/usr/bin/env python3
"""AXON Python 绑定全模块综合演示。

覆盖:
  1. Backtest —— L1MatchingEngine + BacktestEngine 事件驱动
  2. Risk —— DefaultRiskEngine + CircuitBreaker + RiskMetrics
  3. OMS —— OrderManager 订单生命周期 + Portfolio
  4. Exchange —— ExchangeConfig + BinanceAdapter + OrderLifecycleManager
  5. Inference —— ModelConfig + Device + Observation + Action + create_onnx_engine
  6. LLM Trading —— MockTradingBackend + PlaceOrderTool + QueryPortfolioTool + CancelOrderTool + TradingMetrics

运行方式:
    source .venv/bin/activate
    python examples/17_python_bindings/python_bindings_demo.py
"""

from __future__ import annotations

import os
import sys
from typing import Any

RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
RED = "\033[31m"
GREEN = "\033[32m"
YELLOW = "\033[33m"
CYAN = "\033[36m"

if sys.platform == "win32":
    try:
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


def demo_backtest() -> None:
    header("Backtest —— 撮合引擎 + 事件驱动回测", "📈")

    from axon_quant.backtest import (
        L1MatchingEngine, BacktestEngine,
        limit_order, market_order,
        spot_instrument, swap_instrument,  # 0.5.0+:Instrument 工厂,替代 "BTC-USDT" 字符串
    )

    # 0.5.0+ 多 leg 路径统一用 `spot_instrument` / `swap_instrument` 工厂
    btc_spot = spot_instrument("BTC", "USDT")
    eth_spot = spot_instrument("ETH", "USDT")

    step(1, "L1MatchingEngine 基础撮合")
    engine = L1MatchingEngine()
    engine.submit(limit_order(1, btc_spot, "Sell", 100.0, 1.0))
    result = engine.submit(limit_order(2, btc_spot, "Buy", 100.0, 1.0))
    value("is_filled", result["is_filled"])
    value("fills 数量", len(result["fills"]))
    value("成交价", result["fills"][0]["price"])
    value("成交量", result["fills"][0]["quantity"])
    ok("限价单撮合成功")

    step(2, "市价单撮合")
    engine2 = L1MatchingEngine()
    engine2.submit(limit_order(10, eth_spot, "Sell", 3000.0, 5.0))
    result2 = engine2.submit(market_order(11, eth_spot, "Buy", 2.0))
    value("is_filled", result2["is_filled"])
    value("remaining_quantity", result2["remaining_quantity"])
    value("成交价", result2["fills"][0]["price"])
    ok("市价单以 maker 挂单价格成交")

    step(3, "BacktestEngine 事件驱动回测")
    bt = BacktestEngine(initial_cash=100_000.0)
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 1_000,
        "order": limit_order(1, btc_spot, "Sell", 50_000.0, 0.5),
    })
    bt.push_event({
        "type": "order_submitted",
        "timestamp_ns": 2_000,
        "order": market_order(2, btc_spot, "Buy", 0.5),
    })
    run_result = bt.run()
    value("events_processed", run_result.events_processed)
    value("fills", run_result.fills)
    value("final_nav", f"{run_result.final_nav:,.2f}")
    value("total_pnl", f"{run_result.total_pnl:,.2f}")
    ok("事件驱动回测完成")

    step(4, "工厂函数参数对比")
    lo = limit_order(1, btc_spot, "Buy", 50_000.0, 0.1, "IOC")
    mo = market_order(2, eth_spot, "Sell", 1.0)
    value("limit_order tif", lo["tif"])
    value("market_order tif", mo["tif"])
    info("limit_order 默认 GTC，market_order 强制 IOC")

    separator()
    ok("Backtest 模块演示完成\n")


def demo_risk() -> None:
    header("Risk —— 预交易风控 + 熔断器 + 风险指标", "🛡️")

    from axon_quant.risk import (
        DefaultRiskEngine, CircuitBreaker,
        make_order, make_portfolio, make_risk_config, make_circuit_breaker,
    )

    step(1, "创建风控引擎")
    config = make_risk_config(
        max_order_value=10_000.0,
        max_leverage=2.0,
        max_daily_loss=5_000.0,
        max_concentration=0.30,
    )
    engine = DefaultRiskEngine(config)
    portfolio = make_portfolio(
        base_currency="USD", commission_rate=0.001,
        cash={"USD": 100_000.0},
    )
    order = make_order(
        id=1, symbol="BTC-USDT", side="Buy",
        type="limit", price=50_000.0, quantity=0.1,
    )
    value("单笔最大价值", "10,000 USDT")
    value("最大杠杆", "2.0x")
    value("日内最大亏损", "5,000 USDT")
    ok("风控引擎就绪")

    step(2, "正常订单通过风控")
    result = engine.check_order(order, portfolio)
    value("is_allow", result.is_allow)
    value("is_reject", result.is_reject)
    ok("小额订单通过")

    step(3, "超额订单被拒")
    huge_order = make_order(
        id=2, symbol="BTC-USDT", side="Buy",
        type="limit", price=50_000.0, quantity=1.0,
    )
    result2 = engine.check_order(huge_order, portfolio)
    value("is_allow", result2.is_allow)
    value("is_reject", result2.is_reject)
    if result2.is_reject:
        value("拒绝原因", result2.reason.kind)
    ok("超额订单被风控拦截")

    step(4, "日内亏损触发熔断")
    engine.update_daily_pnl(-3_000.0)
    info("累计亏损 -3,000（阈值 5,000，未触发）")
    r1 = engine.check_order(order, portfolio)
    value("is_allow", r1.is_allow)

    engine.update_daily_pnl(-3_000.0)
    info("累计亏损 -6,000（超过阈值，触发熔断）")
    r2 = engine.check_order(order, portfolio)
    value("is_allow", r2.is_allow)
    if r2.is_reject:
        value("拒绝原因", r2.reason.kind)
    ok("熔断器触发成功")

    step(5, "重置日内状态 + 风险指标")
    engine.reset_daily()
    r3 = engine.check_order(order, portfolio)
    value("重置后 is_allow", r3.is_allow)

    metrics = engine.metrics(portfolio)
    separator()
    for k, v in metrics.items():
        if isinstance(v, float):
            value(k, f"{v:,.4f}")
        else:
            value(k, v)
    separator()
    ok("风险指标包含暴露度、杠杆、回撤、VaR 等")

    step(6, "独立 CircuitBreaker")
    cb = make_circuit_breaker(daily_loss_limit=10_000.0, cooldown_seconds=3600)
    cb.check_and_trigger(-5_000.0)
    value("-5,000 触发后", f"active={cb.is_active}")
    cb.check_and_trigger(-15_000.0)
    value("-15,000 触发后", f"active={cb.is_active}")
    cb.reset()
    value("reset 后", f"active={cb.is_active}")
    ok("独立熔断器演示完成")

    separator()
    ok("Risk 模块演示完成\n")


def demo_oms() -> None:
    header("OMS —— 订单生命周期管理", "📋")

    from axon_quant.oms import (
        OrderManager, limit_order, market_order, make_order_status,
        Portfolio, OmsError,
    )

    step(1, "创建 OrderManager + 入金")
    mgr = OrderManager()
    mgr.deposit("USDT", 100_000.0)
    mgr.deposit("BTC", 1.0)
    snap = mgr.snapshot_balance()
    value("USDT 余额", f"{float(snap['cash'].get('USDT', 0)):,.2f}")
    value("BTC 余额", f"{float(snap['cash'].get('BTC', 0)):,.4f}")
    ok("入金成功")

    step(2, "提交限价订单")
    oid = mgr.submit(limit_order(
        "BTC-USDT", "Buy", 0.1, 50_000,
        idempotency_key="demo-001",
    ))
    value("订单 ID", oid)
    status = mgr.get_order_status(oid)
    value("初始状态", status.kind)
    ok("订单提交 → Submitted")

    step(3, "状态流转: Submitted → Acknowledged → PartiallyFilled → Filled")
    mgr.update_status(oid, make_order_status("Acknowledged"))
    value("当前状态", mgr.get_order_status(oid).kind)

    mgr.add_fill(
        order_id=oid, fill_id="fill-001",
        symbol="BTC-USDT", price=50_000.0, quantity=0.05, fee=0.25,
    )
    value("当前状态", mgr.get_order_status(oid).kind)
    value("已成交量", mgr.get_order_status(oid).filled_qty)

    mgr.add_fill(
        order_id=oid, fill_id="fill-002",
        symbol="BTC-USDT", price=50_000.0, quantity=0.05, fee=0.25,
    )
    value("最终状态", mgr.get_order_status(oid).kind)
    ok("订单完全成交: Filled")

    step(4, "Portfolio 快照")
    snap_final = mgr.snapshot_balance()
    value("USDT 余额", f"{float(snap_final['cash'].get('USDT', 0)):,.2f}")
    positions = snap_final.get("positions", {})
    value("持仓数量", len(positions))
    for sym, pos in positions.items():
        info(f"  {sym}: qty={pos.quantity}, avg_price={pos.avg_price}")
    ok("Portfolio 自动更新")

    step(5, "批量提交 + 撤单")
    oids = mgr.batch_submit([
        limit_order("ETH-USDT", "Buy", 1.0, 3_000, idempotency_key="batch-1"),
        limit_order("SOL-USDT", "Buy", 10, 100, idempotency_key="batch-2"),
    ])
    value("批量提交数量", len(oids))
    value("活跃订单数", mgr.active_count())

    cancel_id = oids[0]
    mgr.update_status(cancel_id, make_order_status("Acknowledged"))
    mgr.update_status(cancel_id, make_order_status("Cancelled", filled_qty=0))
    value("撤单后状态", mgr.get_order_status(cancel_id))
    value("历史订单数", mgr.history_count())
    ok("批量提交 + 撤单完成")

    step(6, "独立 Portfolio 类")
    p = Portfolio()
    p.deposit("USDT", 50_000)
    p.apply_fill(fill_id="f1", symbol="BTC-USDT", price=50_000, quantity=0.1, fee=0)
    value("USDT 余额", p.cash["USDT"])
    value("持仓数", p.position_count())
    d = p.to_dict()
    value("to_dict keys", list(d.keys()))
    ok("独立 Portfolio 演示完成")

    separator()
    ok("OMS 模块演示完成\n")


def demo_exchange() -> None:
    header("Exchange —— 交易所适配器 + 订单生命周期", "🏦")

    from axon_quant.exchange import (
        ExchangeId, ExchangeConfig, BinanceAdapter,
        binance_testnet_config, RateLimitConfig, ReconnectConfig,
        OrderLifecycleManager,
    )

    step(1, "ExchangeId 枚举")
    value("Binance", ExchangeId.Binance)
    value("Okx", ExchangeId.Okx)
    ok("交易所 ID 展示")

    step(2, "ExchangeConfig 工厂函数")
    os.environ.setdefault("BINANCE_API_KEY", "demo_key")
    os.environ.setdefault("BINANCE_API_SECRET", "demo_secret")
    config = binance_testnet_config()
    value("repr", repr(config))
    info("API secret 永远不会出现在 repr 中")
    ok("Binance testnet 配置创建成功")

    step(3, "RateLimitConfig + ReconnectConfig")
    rl = RateLimitConfig()
    value("RateLimitConfig", repr(rl))
    rc = ReconnectConfig(max_retries=5)
    value("ReconnectConfig", repr(rc))
    ok("限流 + 重连配置就绪")

    step(4, "BinanceAdapter 创建（不连接）")
    adapter = BinanceAdapter(config)
    value("repr", repr(adapter))
    info("adapter 已创建，未调用 connect()（避免真实网络请求）")
    ok("BinanceAdapter 实例化成功")

    step(5, "OrderLifecycleManager 订单状态跟踪")
    olm = OrderLifecycleManager()
    oid = olm.register_order({
        "symbol": "BTC-USDT", "side": "buy",
        "type": "market", "quantity": "0.1",
        "tif": "IOC", "exchange": "binance",
    })
    value("注册订单 ID", oid)
    value("活跃订单数", olm.active_count())

    olm.update_status(oid, {
        "status": "filled", "filled_qty": "0.1", "avg_price": "50000",
    })
    value("更新后活跃", olm.active_count())
    value("历史订单数", olm.history_count())
    ok("订单状态机: Pending → Filled → 归档")

    separator()
    ok("Exchange 模块演示完成（仅创建，未发起真实连接）\n")


def demo_inference() -> None:
    header("Inference —— 推理引擎配置与创建", "🧠")

    from axon_quant.inference import (
        ModelConfig, Device, Observation, Action, ActionType,
        InferenceBackend, InferenceEngine, InferenceError,
        BatchConfig, InferenceStats, create_onnx_engine,
    )

    step(1, "Device 设备配置")
    cpu = Device.cpu()
    cuda0 = Device.cuda(0)
    metal = Device.metal()
    value("CPU", f"kind={cpu.kind}")
    value("CUDA(0)", f"kind={cuda0.kind}, device_id={cuda0.cuda_device_id}")
    value("Metal", f"kind={metal.kind}")
    ok("三种设备类型展示")

    step(2, "ModelConfig 模型配置")
    cfg = ModelConfig(
        path="/tmp/model.onnx",
        backend=InferenceBackend.Onnx,
        device=Device.cpu(),
        input_shape=(1, 64, 128),
        output_dim=3,
        fp16=False,
        num_threads=4,
    )
    value("path", cfg.path)
    value("backend", cfg.backend)
    value("input_shape", cfg.input_shape)
    value("output_dim", cfg.output_dim)
    value("fp16", cfg.fp16)
    value("num_threads", cfg.num_threads)
    ok("ONNX 模型配置创建成功")

    step(3, "Observation + Action 数据结构")
    features = [0.1 * i for i in range(128)]
    obs = Observation(
        symbol="BTC-USDT",
        timestamp_ns=1_700_000_000_000_000_000,
        features=features,
    )
    value("symbol", obs.symbol)
    value("feature_dim", obs.feature_dim)
    value("features[0:3]", obs.features[:3])

    action = Action(
        action_type="buy", confidence=0.92,
        target_position=0.5, model_id="lstm-v1",
        inference_time_us=320,
    )
    value("action_type", action.action_type)
    value("confidence", f"{action.confidence:.3f}")
    value("target_position", f"{action.target_position:.3f}")
    d = action.to_dict()
    value("to_dict keys", list(d.keys()))
    ok("Observation / Action 创建成功")

    step(4, "ActionType 枚举")
    for at in [ActionType.Buy, ActionType.Sell, ActionType.Hold,
               ActionType.ReduceLong, ActionType.ReduceShort]:
        value(str(at), repr(at))
    ok("5 种动作类型展示")

    step(5, "InferenceEngine 创建（优雅降级）")
    try:
        engine = InferenceEngine(cfg)
        value("backend", engine.backend)
        ok("InferenceEngine 创建成功（模型未加载）")
    except InferenceError as e:
        warn(f"引擎创建失败: {e}")

    step(6, "create_onnx_engine 工厂（无模型文件优雅失败）")
    try:
        engine2 = create_onnx_engine(
            model_path="/tmp/nonexistent.onnx",
            input_shape=(1, 64, 128),
            output_dim=3,
        )
        ok("工厂函数创建成功")
    except InferenceError as e:
        value("错误码", e.args[0])
        value("错误信息", e.args[1][:80])
        info("无模型文件时优雅抛出 InferenceError")
        ok("异常处理符合预期")

    step(7, "InferenceStats 统计")
    stats = InferenceStats()
    value("total_inferences", stats.total_inferences)
    value("avg_latency_us", f"{stats.avg_latency_us:.1f}")
    d = stats.to_dict()
    value("to_dict keys", list(d.keys()))
    ok("InferenceStats 展示完成")

    step(8, "BatchConfig 批推理配置")
    bc = BatchConfig(max_batch_size=32, collect_timeout_us=500, num_workers=2)
    value("max_batch_size", bc.max_batch_size)
    value("collect_timeout_us", bc.collect_timeout_us)
    value("num_workers", bc.num_workers)
    ok("BatchConfig 展示完成")

    separator()
    ok("Inference 模块演示完成\n")


def demo_llm_trading() -> None:
    header("LLM Trading —— Mock 交易后端 + 工具链", "🤖")

    from axon_quant.trading import (
        MockTradingBackend, RiskLimits,
        PlaceOrderTool, QueryPortfolioTool, CancelOrderTool,
        TradingMetrics,
    )

    step(1, "MockTradingBackend + RiskLimits")
    backend = MockTradingBackend()
    risk = RiskLimits(
        max_order_notional=50_000.0,
        max_daily_orders=100,
        max_position_abs=10.0,
        allowed_symbols=["BTC-USDT", "ETH-USDT"],
    )
    value("RiskLimits", repr(risk))
    value("order_count", backend.order_count())
    ok("Mock 后端 + 风控规则就绪")

    step(2, "PlaceOrderTool 下单（DryRun 模式）")
    place = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk)
    result = place.execute({
        "symbol": "BTC-USDT", "side": "Buy",
        "quantity": 0.1, "price": 50_000.0,
    })
    value("status", result["status"])
    value("order_id", result["order_id"])
    value("symbol", result["symbol"])
    value("side", result["side"])
    value("quantity", result["quantity"])
    ok("DryRun 下单成功（不真实执行）")

    step(3, "QueryPortfolioTool 查询组合")
    query = QueryPortfolioTool(backend=backend)
    portfolio = query.execute()
    balance = portfolio.get("balance", {})
    currencies = balance.get("currencies", [])
    value("资产数量", len(currencies))
    for c in currencies:
        if c.get("free", 0) > 0:
            info(f"  {c['currency']}: free={c['free']}, locked={c['locked']}")
    positions = portfolio.get("positions", [])
    value("持仓数量", len(positions))
    for pos in positions:
        info(f"  {pos['symbol']}: qty={pos['quantity']}, entry={pos['entry_price']}")
    ok("组合查询成功")

    step(4, "CancelOrderTool 撤单")
    cancel = CancelOrderTool(backend=backend, risk=risk)
    try:
        cancel_result = cancel.execute({"order_id": "nonexistent-id"})
        value("result", cancel_result)
    except RuntimeError as e:
        info(f"撤单不存在的订单: {str(e)[:60]}")
        ok("撤单异常处理符合预期")

    step(5, "TradingMetrics 指标收集")
    metrics = TradingMetrics()
    snap = metrics.snapshot()
    value("指标数量", len(snap))
    for m in snap:
        info(f"  {m['name']}: kind={m['kind']}, value={m['value']}")
    ok("TradingMetrics 展示完成")

    separator()
    ok("LLM Trading 模块演示完成\n")


def main() -> int:
    import axon_quant
    print(f"""
{BOLD}{CYAN}╔══════════════════════════════════════════════════════════╗
║                                                          ║
║   {BOLD}AXON Quant{RESET}{CYAN}  —  Python 绑定全模块综合演示             ║
║   {DIM}版本: {axon_quant.__version__}{RESET}{CYAN}                                          ║
║                                                          ║
║   {GREEN}Backtest · Risk · OMS · Exchange · Inference · Trading{RESET}{CYAN}  ║
║                                                          ║
╚══════════════════════════════════════════════════════════╝{RESET}
""")

    demos = [
        ("Backtest", demo_backtest),
        ("Risk", demo_risk),
        ("OMS", demo_oms),
        ("Exchange", demo_exchange),
        ("Inference", demo_inference),
        ("LLM Trading", demo_llm_trading),
    ]

    passed = 0
    for name, func in demos:
        try:
            func()
            passed += 1
        except Exception as e:
            fail(f"{name} 演示出错: {e}")
            import traceback
            traceback.print_exc()

    separator()
    ok(f"全部 {passed}/{len(demos)} 个模块演示完成！")
    info("axon_quant 覆盖: 撮合回测 + 风控 + 订单管理 + 交易所 + 推理 + LLM 交易")
    print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
