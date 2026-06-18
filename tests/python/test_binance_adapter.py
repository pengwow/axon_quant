"""Binance 测试网集成测试脚本。

使用 requests 库验证 Binance REST API + 签名逻辑。
验证项：
1. REST 连接（ping）
2. 服务器时间
3. HMAC-SHA256 签名
4. 查询余额（需有效 API key）
5. 查询深度
6. 查询 Ticker
7. 查询 K 线
8. WebSocket 连接

运行方式：
    export BINANCE_TESTNET_API_KEY="your_key"
    export BINANCE_TESTNET_API_SECRET="your_secret"
    .venv/bin/python tests/python/test_binance_adapter.py
"""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import sys
import time

import requests

API_KEY = os.environ.get("BINANCE_TESTNET_API_KEY", "")
API_SECRET = os.environ.get("BINANCE_TESTNET_API_SECRET", "")

# 代理配置：优先使用环境变量
HTTP_PROXY = os.environ.get("https_proxy") or os.environ.get("http_proxy") or ""
ALL_PROXY = os.environ.get("all_proxy") or ""

if not API_KEY or not API_SECRET:
    print("请设置环境变量：")
    print("  export BINANCE_TESTNET_API_KEY='your_key'")
    print("  export BINANCE_TESTNET_API_SECRET='your_secret'")
    sys.exit(1)

TESTNET = "https://testnet.binance.vision"
TIMEOUT = 10
passed = 0
failed = 0
skipped = 0

proxies = {}
if HTTP_PROXY:
    proxies = {"http": HTTP_PROXY, "https": HTTP_PROXY}
elif ALL_PROXY:
    proxies = {"http": ALL_PROXY, "https": ALL_PROXY}


def test(name: str, fn):
    global passed, failed, skipped
    try:
        fn()
        print(f"  ✅ {name}")
        passed += 1
    except (requests.exceptions.ConnectionError, requests.exceptions.SSLError) as e:
        err_str = str(e)
        if "169.254.0.2" in err_str or "timeout" in err_str.lower() or "SSL" in err_str:
            print(f"  ⏭️  {name}: 网络不可达 (需代理: export https_proxy=http://127.0.0.1:7897)")
            skipped += 1
        else:
            print(f"  ❌ {name}: {e}")
            failed += 1
    except Exception as e:
        print(f"  ❌ {name}: {e}")
        failed += 1


def sign(query: str) -> str:
    return hmac.new(API_SECRET.encode(), query.encode(), hashlib.sha256).hexdigest()


def signed_get(path: str, params: str = "") -> dict:
    ts = int(time.time() * 1000)
    q = f"{params}&timestamp={ts}" if params else f"timestamp={ts}"
    sig = sign(q)
    headers = {"X-MBX-APIKEY": API_KEY}
    resp = requests.get(f"{TESTNET}{path}?{q}&signature={sig}", headers=headers, timeout=TIMEOUT, proxies=proxies)
    return resp.json()


def unsigned_get(path: str, params: str = "") -> dict:
    url = f"{TESTNET}{path}" + (f"?{params}" if params else "")
    resp = requests.get(url, timeout=TIMEOUT, proxies=proxies)
    return resp.json()


# ── 1. REST 连接 ──
def test_ping():
    data = unsigned_get("/api/v3/ping")
    assert data == {}

test("REST 连接 (ping)", test_ping)


# ── 2. 服务器时间 ──
def test_server_time():
    data = unsigned_get("/api/v3/time")
    assert "serverTime" in data
    server_time = data["serverTime"]
    local_time = int(time.time() * 1000)
    diff = abs(server_time - local_time)
    print(f"    时差: {diff}ms")
    assert diff < 60000, f"时差过大: {diff}ms"

test("服务器时间", test_server_time)


# ── 3. HMAC 签名 ──
def test_signature():
    query = "timestamp=1234567890"
    sig = sign(query)
    assert len(sig) == 64
    assert all(c in "0123456789abcdef" for c in sig)

test("HMAC-SHA256 签名", test_signature)


# ── 4. 查询余额 ──
def test_balance():
    try:
        account = signed_get("/api/v3/account")
        assert "balances" in account
        balances = [b for b in account["balances"]
                     if float(b["free"]) > 0 or float(b["locked"]) > 0]
        print(f"    非零资产: {len(balances)} 个")
        for b in balances[:3]:
            print(f"      {b['asset']}: free={b['free']}, locked={b['locked']}")
    except requests.exceptions.HTTPError as e:
        if e.response.status_code == 401:
            raise AssertionError("API key 认证失败 (401)")
        raise

test("查询账户余额", test_balance)


# ── 5. 查询深度 ──
def test_depth():
    data = unsigned_get("/api/v3/depth", "symbol=BTCUSDT&limit=5")
    assert "bids" in data
    assert "asks" in data
    assert len(data["bids"]) > 0
    assert len(data["asks"]) > 0
    print(f"    买盘: {len(data['bids'])} 层, 卖盘: {len(data['asks'])} 层")

test("查询深度快照 (BTCUSDT)", test_depth)


# ── 6. 查询 Ticker ──
def test_ticker():
    data = unsigned_get("/api/v3/ticker/24hr", "symbol=BTCUSDT")
    assert "lastPrice" in data
    assert "bidPrice" in data
    assert "askPrice" in data
    print(f"    最新价: {data['lastPrice']}, 买一: {data['bidPrice']}, 卖一: {data['askPrice']}")

test("查询 24hr Ticker", test_ticker)


# ── 7. 查询 K 线 ──
def test_klines():
    data = unsigned_get("/api/v3/klines", "symbol=BTCUSDT&interval=1m&limit=3")
    assert len(data) == 3
    print(f"    最近 3 根 1m K 线:")
    for k in data[:3]:
        print(f"      O={k[1]} H={k[2]} L={k[3]} C={k[4]} V={k[5]}")

test("查询 K 线 (1m)", test_klines)


# ── 8. WebSocket 连接验证 ──
def test_ws_connect():
    import socket
    import ssl

    host = "testnet.binance.vision"
    port = 443
    sock = socket.create_connection((host, port), timeout=5)
    context = ssl.create_default_context()
    ssock = context.wrap_socket(sock, server_hostname=host)
    assert ssock.version() is not None
    print(f"    TLS 版本: {ssock.version()}")
    ssock.close()

test("WebSocket TLS 连接", test_ws_connect)


# ── 汇总 ──
print()
print("=" * 60)
print(f"结果: {passed} passed, {failed} failed, {skipped} skipped (网络不可达)")
print("=" * 60)
if skipped > 0:
    print("提示: 请设置代理后重试: export https_proxy=http://127.0.0.1:7897")

sys.exit(1 if failed > 0 else 0)
