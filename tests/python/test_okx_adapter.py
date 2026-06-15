"""OKX 测试网集成测试脚本。

使用 requests 库验证 OKX REST API + 签名逻辑。
验证项：
1. REST 连接（服务器时间）
2. HMAC-SHA256 签名（OKX 格式）
3. 查询余额
4. 查询深度
5. 查询 Ticker
6. 查询 K 线
7. 查询持仓

运行方式：
    export OKX_TESTNET_API_KEY="your_key"
    export OKX_TESTNET_API_SECRET="your_secret"
    export OKX_TESTNET_PASSPHRASE="your_passphrase"
    .venv/bin/python tests/python/test_okx_adapter.py
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import sys
import time

import requests

API_KEY = os.environ.get("OKX_TESTNET_API_KEY", "")
API_SECRET = os.environ.get("OKX_TESTNET_API_SECRET", "")
PASSPHRASE = os.environ.get("OKX_TESTNET_PASSPHRASE", "")

# 代理配置：优先使用环境变量，否则使用默认值
HTTP_PROXY = os.environ.get("https_proxy") or os.environ.get("http_proxy") or ""
ALL_PROXY = os.environ.get("all_proxy") or ""

if not API_KEY or not API_SECRET:
    print("请设置环境变量：")
    print("  export OKX_TESTNET_API_KEY='your_key'")
    print("  export OKX_TESTNET_API_SECRET='your_secret'")
    print("  export OKX_TESTNET_PASSPHRASE='your_passphrase'")
    sys.exit(1)

BASE = "https://www.okx.com"
TIMEOUT = 10  # 秒
passed = 0
failed = 0
skipped = 0

# 构造代理配置
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
            print(f"  ⏭️  {name}: 网络不可达 (OKX 被墙或 SSL 错误)")
            skipped += 1
        else:
            print(f"  ❌ {name}: {e}")
            failed += 1
    except Exception as e:
        print(f"  ❌ {name}: {e}")
        failed += 1


def sign(timestamp: str, method: str, path: str, body: str = "") -> str:
    """OKX 签名：base64(HMAC-SHA256(timestamp + method + path + body, secret))"""
    prehash = f"{timestamp}{method}{path}{body}"
    mac = hmac.new(API_SECRET.encode(), prehash.encode(), hashlib.sha256)
    return base64.b64encode(mac.digest()).decode()


def signed_get(path: str) -> dict:
    ts = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())
    sig = sign(ts, "GET", path)
    headers = {
        "OK-ACCESS-KEY": API_KEY,
        "OK-ACCESS-SIGN": sig,
        "OK-ACCESS-TIMESTAMP": ts,
        "OK-ACCESS-PASSPHRASE": PASSPHRASE,
        "x-simulated-trading": "1",  # OKX 测试网（模拟盘）
    }
    resp = requests.get(f"{BASE}{path}", headers=headers, timeout=TIMEOUT, proxies=proxies)
    return resp.json()


def unsigned_get(path: str) -> dict:
    resp = requests.get(f"{BASE}{path}", timeout=TIMEOUT, proxies=proxies)
    return resp.json()


# ── 1. 服务器时间 ──
def test_server_time():
    data = unsigned_get("/api/v5/public/time")
    assert data["code"] == "0", f"code={data['code']}"
    server_ts = int(data["data"][0]["ts"]) / 1000
    local_ts = time.time()
    diff = abs(server_ts - local_ts)
    print(f"    时差: {diff:.1f}s")
    assert diff < 60, f"时差过大: {diff}s"

test("服务器时间", test_server_time)


# ── 2. HMAC 签名 ──
def test_signature():
    ts = "2024-01-01T00:00:00.000Z"
    sig = sign(ts, "GET", "/api/v5/account/balance")
    decoded = base64.b64decode(sig)
    assert len(decoded) == 32
    print(f"    签名: {sig[:20]}... (base64, {len(sig)} chars)")

test("HMAC-SHA256 签名 (OKX 格式)", test_signature)


# ── 3. 查询余额 ──
def test_balance():
    data = signed_get("/api/v5/account/balance")
    assert data["code"] == "0", f"code={data['code']}, msg={data.get('msg','')}"
    details = data["data"][0].get("details", [])
    non_zero = [d for d in details if float(d.get("availBal", "0")) > 0 or float(d.get("frozenBal", "0")) > 0]
    print(f"    非零资产: {len(non_zero)} 个")
    for d in non_zero[:5]:
        print(f"      {d['ccy']}: avail={d.get('availBal','0')}, frozen={d.get('frozenBal','0')}")

test("查询账户余额", test_balance)


# ── 4. 查询深度 ──
def test_depth():
    data = unsigned_get("/api/v5/market/books?instId=BTC-USDT&sz=5")
    assert data["code"] == "0"
    book = data["data"][0]
    bids = book.get("bids", [])
    asks = book.get("asks", [])
    print(f"    买盘: {len(bids)} 层, 卖盘: {len(asks)} 层")
    if bids:
        print(f"    最高买: {bids[0][0]} × {bids[0][1]}")
    if asks:
        print(f"    最低卖: {asks[0][0]} × {asks[0][1]}")

test("查询深度 (BTC-USDT)", test_depth)


# ── 5. 查询 Ticker ──
def test_ticker():
    data = unsigned_get("/api/v5/market/ticker?instId=BTC-USDT")
    assert data["code"] == "0"
    t = data["data"][0]
    print(f"    最新价: {t['last']}")
    print(f"    买一:   {t['bidPx']}")
    print(f"    卖一:   {t['askPx']}")
    print(f"    24h 成交量: {t['vol24h']}")

test("查询 Ticker (BTC-USDT)", test_ticker)


# ── 6. 查询 K 线 ──
def test_klines():
    data = unsigned_get("/api/v5/market/candles?instId=BTC-USDT&bar=1m&limit=3")
    assert data["code"] == "0"
    candles = data["data"]
    print(f"    最近 {len(candles)} 根 1m K 线:")
    for c in candles[:3]:
        print(f"      O={c[1]} H={c[2]} L={c[3]} C={c[4]} V={c[5]}")

test("查询 K 线 (1m)", test_klines)


# ── 7. 查询持仓 ──
def test_positions():
    data = signed_get("/api/v5/account/positions")
    assert data["code"] == "0"
    positions = data["data"]
    print(f"    持仓数: {len(positions)}")
    for p in positions[:3]:
        print(f"      {p['instId']}: {p['posSide']} {p['pos']} @ {p.get('avgPx', 'N/A')}")

test("查询持仓", test_positions)


# ── 汇总 ──
print()
print("=" * 60)
print(f"结果: {passed} passed, {failed} failed, {skipped} skipped (网络不可达)")
print("=" * 60)
if skipped > 0:
    print("提示: OKX 在当前网络不可达，请使用 VPN 或代理后重试")

sys.exit(1 if failed > 0 else 0)
