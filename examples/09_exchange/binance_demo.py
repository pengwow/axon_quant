"""binance_demo.py — Binance 测试网对接示例。

演示 Binance REST API 基本操作：
1. 连接验证
2. 查询余额 / 深度 / Ticker
3. HMAC-SHA256 签名
4. WebSocket 行情模拟

运行方式：
    export BINANCE_TESTNET_API_KEY="your_key"
    export BINANCE_TESTNET_API_SECRET="your_secret"
    .venv/bin/python examples/09_exchange/binance_demo.py
"""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import sys
import time
import urllib.request

API_KEY = os.environ.get("BINANCE_TESTNET_API_KEY", "")
API_SECRET = os.environ.get("BINANCE_TESTNET_API_SECRET", "")

if not API_KEY or not API_SECRET:
    print("请设置环境变量：")
    print("  export BINANCE_TESTNET_API_KEY='your_key'")
    print("  export BINANCE_TESTNET_API_SECRET='your_secret'")
    sys.exit(1)

TESTNET = "https://testnet.binance.vision"


def sign(query: str) -> str:
    return hmac.new(API_SECRET.encode(), query.encode(), hashlib.sha256).hexdigest()


def signed_get(path: str, params: str = "") -> dict:
    ts = int(time.time() * 1000)
    q = f"{params}&timestamp={ts}" if params else f"timestamp={ts}"
    sig = sign(q)
    url = f"{TESTNET}{path}?{q}&signature={sig}"
    req = urllib.request.Request(url)
    req.add_header("X-MBX-APIKEY", API_KEY)
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())


def unsigned_get(path: str, params: str = "") -> dict:
    url = f"{TESTNET}{path}" + (f"?{params}" if params else "")
    req = urllib.request.Request(url)
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())


def main() -> int:
    print("=" * 60)
    print("Binance 测试网对接示例")
    print("=" * 60)

    # 1. 连接验证
    print("\n[1] REST 连接验证")
    data = unsigned_get("/api/v3/ping")
    print(f"  ✅ ping 响应: {data}")

    # 2. 服务器时间
    print("\n[2] 服务器时间")
    data = unsigned_get("/api/v3/time")
    server_time = data["serverTime"]
    local_time = int(time.time() * 1000)
    print(f"  服务器: {server_time}")
    print(f"  本地:   {local_time}")
    print(f"  时差:   {abs(server_time - local_time)}ms")

    # 3. 查询余额
    print("\n[3] 账户余额")
    try:
        account = signed_get("/api/v3/account")
        balances = [b for b in account["balances"]
                     if float(b["free"]) > 0 or float(b["locked"]) > 0]
        print(f"  非零资产: {len(balances)} 个")
        for b in balances[:5]:
            print(f"    {b['asset']:>6s}: free={b['free']}, locked={b['locked']}")
    except Exception as e:
        print(f"  ⚠️  查询失败: {e}")

    # 4. 查询深度
    print("\n[4] BTCUSDT 深度")
    depth = unsigned_get("/api/v3/depth", "symbol=BTCUSDT&limit=5")
    print("  买盘 (Top 5):")
    for price, qty in depth["bids"][:5]:
        print(f"    {price:>12s} × {qty}")
    print("  卖盘 (Top 5):")
    for price, qty in depth["asks"][:5]:
        print(f"    {price:>12s} × {qty}")

    # 5. 查询 Ticker
    print("\n[5] BTCUSDT 24hr Ticker")
    ticker = unsigned_get("/api/v3/ticker/24hr", "symbol=BTCUSDT")
    print(f"  最新价:   {ticker['lastPrice']}")
    print(f"  买一价:   {ticker['bidPrice']}")
    print(f"  卖一价:   {ticker['askPrice']}")
    print(f"  24h 成交量: {ticker['volume']}")
    print(f"  24h 涨跌: {ticker['priceChangePercent']}%")

    # 6. K 线
    print("\n[6] BTCUSDT K 线 (最近 3 根 1m)")
    klines = unsigned_get("/api/v3/klines", "symbol=BTCUSDT&interval=1m&limit=3")
    for k in klines[:3]:
        print(f"  O={k[1]} H={k[2]} L={k[3]} C={k[4]} V={k[5]}")

    # 7. 签名演示
    print("\n[7] HMAC-SHA256 签名演示")
    ts = int(time.time() * 1000)
    query = f"timestamp={ts}"
    sig = sign(query)
    print(f"  查询: {query}")
    print(f"  签名: {sig}")

    # 8. WebSocket 消息格式
    print("\n[8] WebSocket 推送消息格式")
    samples = [
        {"type": "Ticker", "example": '{"e":"24hrTicker","s":"BTCUSDT","b":"50000","a":"50001","c":"50000.5"}'},
        {"type": "Depth", "example": '{"e":"depthUpdate","s":"BTCUSDT","b":[["50000","1"]],"a":[["50001","0.5"]]}'},
        {"type": "Trade", "example": '{"e":"trade","s":"BTCUSDT","p":"50000","q":"0.1","m":false}'},
    ]
    for s in samples:
        print(f"  {s['type']}: {s['example'][:60]}...")

    print("\n" + "=" * 60)
    print("✅ Binance 测试网对接示例完成")
    print("=" * 60)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
