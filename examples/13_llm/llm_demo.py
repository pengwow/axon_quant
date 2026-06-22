#!/usr/bin/env python3
"""Axon LLM + Trading 交易 Agent 演示。

展示 LLM 驱动的量化交易工作流:
  1. 市场分析 → LLM 生成交易信号
  2. 信号 → 风控检查 → Mock 后端执行
  3. 执行结果 → LLM 总结复盘

需要网络和 API Key:
  export AXON_LLM_BASE_URL="https://api.openai.com/v1"
  export AXON_LLM_API_KEY="sk-xxx"
  export AXON_LLM_MODEL="gpt-4o-mini"

运行方式:
    source .venv/bin/activate
    python examples/13_llm/llm_demo.py
"""

from __future__ import annotations

import json
import os
import sys
import time
from typing import Any

RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
RED = "\033[31m"
GREEN = "\033[32m"
YELLOW = "\033[33m"
BLUE = "\033[34m"
MAGENTA = "\033[35m"
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


def resolve_env_config() -> dict[str, str]:
    base_url = os.environ.get("AXON_LLM_BASE_URL", "https://api.openai.com/v1")
    api_key = os.environ.get("AXON_LLM_API_KEY", "")
    model = os.environ.get("AXON_LLM_MODEL", "gpt-4o-mini")
    if not api_key:
        api_key = os.environ.get("OPENAI_API_KEY", "")
    return {"base_url": base_url, "api_key": api_key, "model": model}


def check_api_key(env: dict[str, str]) -> bool:
    if env["api_key"]:
        return True
    print(f"""
  {RED}未检测到 API Key。{RESET}

  请设置环境变量:

    {BOLD}export AXON_LLM_API_KEY="sk-..."{RESET}
    {BOLD}export OPENAI_API_KEY="sk-..."{RESET}

  可选:
    AXON_LLM_BASE_URL  — API 端点 (默认 https://api.openai.com/v1)
    AXON_LLM_MODEL     — 模型名  (默认 gpt-4o-mini)
""")
    return False


def build_config(env: dict[str, str]) -> Any:
    from axon_quant.llm import LLMConfig
    return LLMConfig(
        backends=[{
            "base_url": env["base_url"],
            "api_key": env["api_key"],
            "model": env["model"],
            "max_tokens": 2048,
            "temperature": 0.3,
            "timeout_secs": 60,
        }],
    )


def fetch_real_klines(symbol: str = "BTCUSDT", interval: str = "1h",
                      limit: int = 100) -> list[dict[str, Any]] | None:
    import urllib.request
    url = f"https://api.binance.com/api/v3/klines?symbol={symbol}&interval={interval}&limit={limit}"
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "axon-quant/1.0"})
        with urllib.request.urlopen(req, timeout=10) as resp:
            raw = json.loads(resp.read())
        bars = []
        for k in raw:
            ts_ms = int(k[0])
            bars.append({
                "timestamp": ts_ms,
                "open": float(k[1]),
                "high": float(k[2]),
                "low": float(k[3]),
                "close": float(k[4]),
                "volume": float(k[5]),
            })
        return bars
    except Exception:
        return None


def make_synthetic_klines(n: int = 100, seed: int = 42) -> list[dict[str, Any]]:
    import random
    rng = random.Random(seed)
    bars = []
    price = 65_000.0
    for i in range(n):
        ret = rng.gauss(0.0, 0.015)
        open_ = price
        close = max(1.0, open_ * (1.0 + ret))
        high = max(open_, close) * (1 + abs(rng.gauss(0, 0.005)))
        low = min(open_, close) * (1 - abs(rng.gauss(0, 0.005)))
        vol = 1_000_000 + rng.gauss(0, 300_000)
        bars.append({
            "timestamp": i,
            "open": round(open_, 2),
            "high": round(high, 2),
            "low": round(low, 2),
            "close": round(close, 2),
            "volume": round(abs(vol), 0),
        })
        price = close
    return bars


def load_klines(n: int = 100) -> tuple[list[dict[str, Any]], str]:
    real = fetch_real_klines(limit=n)
    if real and len(real) >= 20:
        return real, "real"
    return make_synthetic_klines(n), "synthetic"


def _extract_json_objects(text: str) -> list[dict[str, Any]]:
    import re
    cleaned = re.sub(r"```(?:json)?\s*\n(.*?)\n\s*```", r"\1", text, flags=re.DOTALL)

    objects = []
    depth = 0
    start = -1
    for i, ch in enumerate(cleaned):
        if ch == "{":
            if depth == 0:
                start = i
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0 and start >= 0:
                try:
                    obj = json.loads(cleaned[start:i + 1])
                    objects.append(obj)
                except json.JSONDecodeError:
                    pass
                start = -1
    return objects


TRADING_TOOLS_SCHEMA = [
    {
        "type": "function",
        "function": {
            "name": "place_order",
            "description": "提交限价或市价订单到交易所",
            "parameters": {
                "type": "object",
                "properties": {
                    "symbol": {"type": "string", "description": "交易对，如 BTC-USDT"},
                    "side": {"type": "string", "enum": ["Buy", "Sell"], "description": "买卖方向"},
                    "quantity": {"type": "number", "description": "下单数量"},
                    "price": {"type": "number", "description": "限价单价，市价单可省略"},
                    "order_type": {"type": "string", "enum": ["limit", "market"], "description": "订单类型"},
                },
                "required": ["symbol", "side", "quantity", "order_type"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "query_portfolio",
            "description": "查询当前账户持仓和余额",
            "parameters": {"type": "object", "properties": {}},
        },
    },
    {
        "type": "function",
        "function": {
            "name": "cancel_order",
            "description": "撤销指定订单",
            "parameters": {
                "type": "object",
                "properties": {
                    "order_id": {"type": "string", "description": "要撤销的订单 ID"},
                },
                "required": ["order_id"],
            },
        },
    },
]


def simulate_tool_call(tool_name: str, tool_args: dict[str, Any],
                       tools: dict[str, Any]) -> str:
    place_tool = tools["place"]
    query_tool = tools["query"]
    cancel_tool = tools["cancel"]

    if tool_name == "place_order":
        args = dict(tool_args)
        if "order_type" in args:
            ot = args["order_type"]
            args["order_type"] = ot.capitalize() if ot.lower() in ("limit", "market") else ot
        if "side" in args:
            s = args["side"]
            args["side"] = s.capitalize() if s.lower() in ("buy", "sell") else s
        result = place_tool.execute(args)
        return json.dumps(result, ensure_ascii=False)
    elif tool_name == "query_portfolio":
        result = query_tool.execute()
        return json.dumps(result, ensure_ascii=False)
    elif tool_name == "cancel_order":
        result = cancel_tool.execute(tool_args)
        return json.dumps(result, ensure_ascii=False)
    else:
        return json.dumps({"error": f"unknown tool: {tool_name}"})


def demo_market_analysis(backend: Any) -> list[dict[str, Any]]:
    header("场景 1: LLM 市场分析 → 生成交易信号", "📊")

    from axon_quant.llm import LLMMessage

    klines, source = load_klines(100)
    recent = klines[-5:]

    step(1, "加载市场数据")
    if source == "real":
        from datetime import datetime, timezone
        ts0 = klines[0]["timestamp"]
        ts1 = klines[-1]["timestamp"]
        t0_str = datetime.fromtimestamp(ts0 / 1000, tz=timezone.utc).strftime("%Y-%m-%d %H:%M")
        t1_str = datetime.fromtimestamp(ts1 / 1000, tz=timezone.utc).strftime("%Y-%m-%d %H:%M")
        value("数据来源", "Binance 真实行情 (1h)")
        value("时间范围", f"{t0_str} ~ {t1_str} UTC")
    else:
        value("数据来源", "合成数据 (离线模式)")
        value("数据范围", f"第 {klines[0]['timestamp']} ~ {klines[-1]['timestamp']} 根")
    value("K 线数量", len(klines))
    value("最新价", f"${klines[-1]['close']:,.2f}")
    value("涨跌幅", f"{(klines[-1]['close'] / klines[0]['close'] - 1) * 100:+.2f}%")
    ok("数据就绪")

    step(2, "发送市场数据给 LLM 分析")
    recent = klines[-5:]
    kline_str = "\n".join(
        f"  {b['timestamp']}: O={b['open']:>10.2f} H={b['high']:>10.2f} "
        f"L={b['low']:>10.2f} C={b['close']:>10.2f} V={b['volume']:>12.0f}"
        for b in recent
    )
    system_prompt = (
        "你是量化交易分析师。分析 K 线数据后，输出 JSON 格式的交易决策。\n"
        "输出格式: {\"action\": \"Buy\"|\"Sell\"|\"Hold\", \"symbol\": \"BTC-USDT\", "
        "\"quantity\": 0.001, \"price\": 65000, \"order_type\": \"Limit\"|\"Market\", "
        "\"confidence\": 0.8, \"reason\": \"简要分析原因\"}\n"
        "注意: action 和 order_type 首字母必须大写 (Buy/Sell/Limit/Market)。\n"
        "只输出 JSON，不要其他文字。"
    )
    user_msg = f"最近 5 根 K 线:\n{kline_str}\n\n请分析并给出交易决策。"
    messages = [
        LLMMessage("system", system_prompt),
        LLMMessage("user", user_msg),
    ]

    t0 = time.perf_counter()
    result = backend.chat(messages)
    elapsed = time.perf_counter() - t0
    reply = result.get("content", "")
    value("LLM 响应耗时", f"{elapsed:.2f}s")
    value("Token 用量", result.get("total_tokens", "N/A"))

    step(3, "解析 LLM 交易决策")
    print(f"\n    {CYAN}{'─' * 50}{RESET}")
    print(f"    {BOLD}LLM 分析结果:{RESET}")
    for line in reply.strip().split("\n"):
        print(f"    {line}")
    print(f"    {CYAN}{'─' * 50}{RESET}")

    decision = None
    for line in reply.strip().split("\n"):
        line = line.strip()
        if line.startswith("{"):
            try:
                decision = json.loads(line)
                break
            except json.JSONDecodeError:
                continue
    if decision is None:
        try:
            decision = json.loads(reply.strip())
        except json.JSONDecodeError:
            warn("无法解析 LLM 输出为 JSON，使用模拟决策")
            decision = {
                "action": "Buy", "symbol": "BTC-USDT",
                "quantity": 0.001, "price": klines[-1]["close"],
                "confidence": 0.7, "reason": "LLM 输出解析失败，使用默认买入",
            }

    value("决策动作", decision.get("action", "N/A"))
    value("交易对", decision.get("symbol", "N/A"))
    value("数量", decision.get("quantity", "N/A"))
    value("价格", decision.get("price", "N/A"))
    value("置信度", decision.get("confidence", "N/A"))
    value("理由", decision.get("reason", "N/A")[:80])

    separator()
    ok("LLM 市场分析完成\n")
    return [klines, decision]


def demo_signal_to_execution(backend: Any, context: list[Any]) -> dict[str, Any]:
    header("场景 2: 信号 → 风控 → 执行", "⚡")

    from axon_quant.trading import (
        RiskLimits, MockTradingBackend,
        PlaceOrderTool, QueryPortfolioTool, CancelOrderTool, TradingMetrics,
    )
    from axon_quant.risk import DefaultRiskEngine, make_order, make_portfolio, make_risk_config

    klines, decision = context

    step(1, "初始化 Mock 交易所 + 风控")
    backend_trading = MockTradingBackend()
    risk_limits = RiskLimits(
        max_order_notional=10_000.0,
        max_daily_orders=20,
        allowed_symbols=["BTC-USDT", "ETH-USDT"],
    )
    place_tool = PlaceOrderTool(backend=backend_trading, mode="direct", risk=risk_limits)
    query_tool = QueryPortfolioTool(backend=backend_trading)
    cancel_tool = CancelOrderTool(backend=backend_trading, risk=risk_limits)
    metrics = TradingMetrics()

    risk_engine = DefaultRiskEngine(make_risk_config(
        max_order_value=10_000.0,
        max_daily_loss=5_000.0,
    ))
    ok("Mock 交易所 + 风控引擎就绪")

    step(2, "风控预检查")
    action = decision.get("action", "Buy")
    if action.lower() == "hold":
        value("LLM 决策", "Hold (持仓不动)")
        info("LLM 决定不交易，跳过风控和执行")
        separator()
        ok("信号分析完成 — 决策: Hold\n")
        return {"executed": False, "reason": "Hold"}

    order = decision.copy()
    side = action.capitalize()
    if side not in ("Buy", "Sell"):
        warn(f"非法 side={side}，降级为 Buy")
        side = "Buy"
    risk_order = make_order(
        id=1, symbol=order.get("symbol", "BTC-USDT"),
        side=side,
        type=order.get("order_type", "Limit"),
        quantity=order.get("quantity", 0.001),
        price=order.get("price", klines[-1]["close"]),
    )
    portfolio = make_portfolio(base_currency="USDT", cash={"USDT": 100_000.0})
    risk_result = risk_engine.check_order(risk_order, portfolio)
    value("风控结果", "通过" if risk_result.is_allow else f"拒绝: {risk_result.reason}")
    if not risk_result.is_allow:
        warn("风控拒绝，跳过执行")
        return {"executed": False, "reason": str(risk_result.reason)}

    step(3, "执行 LLM 决策")
    tools = {"place": place_tool, "query": query_tool, "cancel": cancel_tool, "risk_limits": risk_limits}
    tool_args = {
        "symbol": order.get("symbol", "BTC-USDT"),
        "side": side,
        "quantity": order.get("quantity", 0.001),
        "price": order.get("price", klines[-1]["close"]),
        "order_type": order.get("order_type", "Limit"),
    }
    value("工具调用", f"place_order({json.dumps(tool_args)})")

    exec_result = simulate_tool_call("place_order", tool_args, tools)
    result_dict = json.loads(exec_result)
    value("执行状态", result_dict.get("status", "N/A"))
    value("订单 ID", result_dict.get("order_id", "N/A"))
    ok("订单执行成功")

    step(4, "查询执行后组合")
    portfolio_result = simulate_tool_call("query_portfolio", {}, tools)
    portfolio_data = json.loads(portfolio_result)
    if "balance" in portfolio_data:
        currencies = portfolio_data["balance"].get("currencies", [])
        for c in currencies:
            if float(c.get("free", 0)) > 0:
                info(f"  {c['currency']}: free={c['free']}, locked={c['locked']}")
    if "positions" in portfolio_data:
        for pos in portfolio_data["positions"]:
            info(f"  {pos['symbol']}: qty={pos['quantity']}, entry={pos['entry_price']}")
    ok("组合状态已更新")

    separator()
    ok("信号 → 执行流程完成\n")
    return {"executed": True, "result": result_dict, "portfolio": portfolio_data}


def demo_react_loop(backend: Any) -> None:
    header("场景 3: ReAct Agent 自主决策循环", "🤖")
    info("ReAct = Reasoning(分析) → Action(执行) → Observation(观察) → Reflection(反思)")
    info("循环迭代直到 LLM 给出最终结论")

    from axon_quant.llm import LLMMessage
    from axon_quant.trading import (
        RiskLimits, MockTradingBackend,
        PlaceOrderTool, QueryPortfolioTool, CancelOrderTool,
    )

    step(1, "初始化交易环境")
    backend_trading = MockTradingBackend()
    risk_limits = RiskLimits(
        max_order_notional=5_000.0,
        max_daily_orders=10,
        allowed_symbols=["BTC-USDT", "ETH-USDT"],
    )
    place_tool = PlaceOrderTool(backend=backend_trading, mode="direct", risk=risk_limits)
    query_tool = QueryPortfolioTool(backend=backend_trading)
    cancel_tool = CancelOrderTool(backend=backend_trading, risk=risk_limits)
    tools = {"place": place_tool, "query": query_tool, "cancel": cancel_tool, "risk_limits": risk_limits}
    ok("交易环境就绪 (MockTradingBackend + RiskLimits)")

    step(2, "启动 ReAct 循环")
    klines, source = load_klines(50)
    if source == "real":
        info("使用 Binance 真实行情数据")
    else:
        info("使用合成数据 (无法获取真实行情)")
    market_ctx = (
        f"BTC-USDT 最近行情:\n"
        f"  当前价: ${klines[-1]['close']:,.2f}\n"
        f"  24h 最高: ${max(b['high'] for b in klines):,.2f}\n"
        f"  24h 最低: ${min(b['low'] for b in klines):,.2f}\n"
        f"  趋势: {'上涨' if klines[-1]['close'] > klines[0]['close'] else '下跌'}\n"
        f"  你的 USDT 余额: 100,000\n"
    )

    system_prompt = (
        "你是量化交易 Agent，使用 ReAct 模式决策。\n"
        "每轮输出顺序:\n"
        "1. {\"reason\": \"分析...\"}\n"
        "2. {\"action\": \"place_order\"|\"query_portfolio\"|\"cancel_order\"|\"hold\"|\"final\", \"args\": {...}}\n"
        "工具: place_order(symbol,side=Buy/Sell,quantity,price,order_type=Limit/Market)\n"
        "     query_portfolio() cancel_order(order_id)\n"
        "action=final 时 args={\"summary\": \"总结\"}，结束循环。\n"
        "每次只调一个工具，只输出 JSON。"
    )

    messages: list[Any] = [
        LLMMessage("system", system_prompt),
        LLMMessage("user", f"市场状态:\n{market_ctx}\n\n开始 ReAct 决策循环。"),
    ]

    max_iterations = 3
    iteration_log: list[dict[str, Any]] = []

    for i in range(max_iterations):
        iter_start = time.perf_counter()
        is_final = False
        reason_text = ""
        action_text = ""
        print(f"\n    {BOLD}{MAGENTA}{'━' * 50}{RESET}")
        print(f"    {BOLD}{MAGENTA}  ⟳ ReAct 轮次 {i + 1}/{max_iterations}{RESET}")
        print(f"    {BOLD}{MAGENTA}{'━' * 50}{RESET}")

        result = backend.chat(messages)
        reply = result.get("content", "").strip()

        print(f"\n    {DIM}  LLM 原始输出:{RESET}")
        if not reply:
            print(f"    {RED}  ⚠️ LLM 返回空内容 (tokens={result.get('total_tokens', '?')}){RESET}")
            messages.append(LLMMessage("user", "你的回复为空，请重新输出 JSON。"))
            continue
        for rline in reply.split("\n")[:15]:
            print(f"    {DIM}  │ {rline}{RESET}")
        if reply.count("\n") > 15:
            print(f"    {DIM}  │ ... ({reply.count(chr(10))} 行){RESET}")

        messages.append(LLMMessage("assistant", reply))

        json_objects = _extract_json_objects(reply)
        if not json_objects:
            print(f"\n    {RED}  ⚠️ 未解析到 JSON 对象，尝试从原始文本推断...{RESET}")
            if "{" in reply and "}" in reply:
                snippet = reply[reply.index("{"):reply.rindex("}") + 1]
                try:
                    json_objects = [json.loads(snippet)]
                except json.JSONDecodeError:
                    print(f"    {RED}  推断失败，结束循环{RESET}")
                    break

        for obj in json_objects:
            if "reason" in obj:
                reason_text = obj["reason"]
                print(f"\n    {BOLD}{BLUE}  🧠 REASON (分析推理):{RESET}")
                for rline in reason_text.split("\n"):
                    print(f"    {BLUE}  {rline}{RESET}")

            elif "action" in obj and "args" in obj:
                action_text = obj["action"]
                action_args = obj["args"]
                print(f"\n    {BOLD}{YELLOW}  ⚡ ACTION (执行动作):{RESET}")
                print(f"    {YELLOW}  → {action_text}{RESET}")
                if action_args:
                    args_str = json.dumps(action_args, ensure_ascii=False, indent=4)
                    for aline in args_str.split("\n"):
                        print(f"    {YELLOW}    {aline}{RESET}")

                if action_text == "final":
                    summary = action_args.get("summary", "")
                    print(f"\n    {BOLD}{GREEN}  📝 FINAL SUMMARY (最终结论):{RESET}")
                    for sline in summary.split("\n"):
                        print(f"    {GREEN}  {sline}{RESET}")
                    is_final = True
                    break

                tool_result = simulate_tool_call(action_text, action_args, tools)
                observation_text = tool_result
                print(f"\n    {BOLD}{CYAN}  👁️ OBSERVATION (观察结果):{RESET}")
                try:
                    parsed = json.loads(tool_result)
                    obs_formatted = json.dumps(parsed, ensure_ascii=False, indent=4)
                    for oline in obs_formatted.split("\n")[:10]:
                        print(f"    {CYAN}  {oline}{RESET}")
                except json.JSONDecodeError:
                    print(f"    {CYAN}  {tool_result[:200]}{RESET}")

                obs_brief = tool_result[:300]
                messages.append(LLMMessage("user", f"Observation: {obs_brief}"))

        if is_final:
            iter_elapsed = time.perf_counter() - iter_start
            iteration_log.append({
                "round": i + 1, "reason": reason_text,
                "action": "final", "time": iter_elapsed,
            })
            break

        if reason_text:
            print(f"\n    {BOLD}{MAGENTA}  🔄 REFLECT → 进入下一轮...{RESET}")
            iter_elapsed = time.perf_counter() - iter_start
            iteration_log.append({
                "round": i + 1, "reason": reason_text[:80],
                "action": action_text, "time": iter_elapsed,
            })

    step(3, "ReAct 循环总结")
    print()
    for entry in iteration_log:
        icon = "📝" if entry["action"] == "final" else "⟳"
        print(f"    {icon} 轮次 {entry['round']}: "
              f"reason=\"{entry['reason']}\" action={entry['action']} "
              f"({entry['time']:.2f}s)")
    value("总迭代轮次", len(iteration_log))
    value("对话消息数", len(messages))

    separator()
    ok("ReAct 循环完成 — 每轮可见: REASON → ACTION → OBSERVE → REFLECT\n")


def demo_risk_enforced_execution() -> None:
    header("场景 4: 风控强制拦截演示", "🛡️")

    from axon_quant.trading import (
        RiskLimits, MockTradingBackend,
        PlaceOrderTool, QueryPortfolioTool,
    )
    from axon_quant.risk import DefaultRiskEngine, make_order, make_portfolio, make_risk_config

    step(1, "配置严格风控: 单笔最大 500 USDT")
    backend_trading = MockTradingBackend()
    risk_limits = RiskLimits(max_order_notional=500.0, max_daily_orders=5, allowed_symbols=["BTC-USDT"])
    place_tool = PlaceOrderTool(backend=backend_trading, mode="direct", risk=risk_limits)
    query_tool = QueryPortfolioTool(backend=backend_trading)
    risk_engine = DefaultRiskEngine(make_risk_config(max_order_value=500.0, max_daily_loss=1_000.0))
    ok("风控: 单笔 ≤ 500 USDT, 日亏 ≤ 1,000 USDT")

    step(2, "模拟 LLM 建议大额买入（应被风控拦截）")
    large_order_args = {
        "symbol": "BTC-USDT", "side": "Buy",
        "quantity": 0.1, "price": 65_000.0, "order_type": "Limit",
    }
    value("LLM 建议", f"买入 0.1 BTC @ $65,000 = $6,500")
    risk_order = make_order(
        id=1, symbol="BTC-USDT", side="Buy", type="limit",
        price=65_000.0, quantity=0.1,
    )
    portfolio = make_portfolio(base_currency="USDT", cash={"USDT": 100_000.0})
    risk_result = risk_engine.check_order(risk_order, portfolio)
    value("风控检查", "通过" if risk_result.is_allow else f"拒绝: {risk_result.reason}")
    if not risk_result.is_allow:
        ok("风控拦截成功: 单笔 6,500 > 500 限制")

    step(3, "调整为合规订单后执行")
    safe_order_args = {
        "symbol": "BTC-USDT", "side": "Buy",
        "quantity": 0.001, "price": 65_000.0, "order_type": "Limit",
    }
    value("调整后", "买入 0.001 BTC @ $65,000 = $65")
    exec_result = place_tool.execute(safe_order_args)
    result_dict = json.loads(exec_result) if isinstance(exec_result, str) else exec_result
    value("执行状态", result_dict.get("status", "N/A"))
    ok("合规订单执行成功")

    step(4, "模拟日内亏损触发熔断")
    risk_engine.update_daily_pnl(-800.0)
    risk_engine.update_daily_pnl(-300.0)
    info("累计日内亏损: -1,100 USDT (超过 1,000 阈值)")
    risk_result2 = risk_engine.check_order(risk_order, portfolio)
    value("风控检查", "通过" if risk_result2.is_allow else f"拒绝: {risk_result2.reason}")
    if not risk_result2.is_allow:
        ok("熔断器触发: 日内亏损超限，拒绝新订单")

    separator()
    ok("风控强制拦截演示完成\n")


def main() -> int:
    header("Axon LLM + Trading 交易 Agent 演示", "🤖")
    info("展示 LLM 驱动的量化交易: 分析 → 决策 → 风控 → 执行 → 复盘")
    info("需要网络和 API Key")

    env = resolve_env_config()
    if not check_api_key(env):
        return 1

    step(0, "初始化 LLM 后端")
    from axon_quant.llm import make_backend
    config = build_config(env)
    backend = make_backend(config)
    value("模型", config.backends[0]["model"])
    value("端点", config.backends[0]["base_url"])
    ok("LLM 后端就绪")

    context = demo_market_analysis(backend)
    demo_signal_to_execution(backend, context)
    demo_react_loop(backend)
    demo_risk_enforced_execution()

    header("演示完成", "🎉")
    ok("LLM + Trading 交易 Agent 完整流程:\n"
       "    市场分析 → 信号生成 → 风控检查 → Mock 执行 → ReAct 循环 → 熔断拦截\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
