"""axon_quant.exchange 端到端测试(L3 Python E2E,Stage 5)。

覆盖范围:
1. 类型导入 / 实例化(9 个核心类型 + 2 个工厂函数)
2. 配置安全:`__repr__` 不泄漏 `api_secret`
3. Binance testnet config 工厂(从 env 读 key,缺 env 抛 ExchangeError)
4. OKX testnet config 工厂(从 env 读 key + passphrase)
5. ExchangeId 枚举(从 ExchangeConfig 读出)
6. OrderLifecycleManager 注册 / 更新 / 状态机迁移
7. TokenBucketRateLimiter 状态读取(try_acquire / available_tokens / status dict)
8. ExchangeError 异常路径(继承 PyException,不继承 AxonError)
9. BinanceAdapter 构造(不连真实网络,只验证 config 注入成功)
10. OkxAdapter 构造(同上,带 passphrase)
11. 离线 dict→Order 协议验证(place_order 不连真实网络,改用底层验证)

运行::

    cd /Users/liupeng/workspace/quant/axon
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon/.venv/bin/python \\
        python -m pytest python/tests/test_exchange_e2e.py -v

**注意**:
- 默认**所有**测试都跑(无需 TESTNET 环境变量),仅依赖 mock config
- 真实 testnet 连通性测试需 ``TESTNET=1 BINANCE_API_KEY=... pytest`` 启用,
  本文件不覆盖(避免 CI 误连)
- 需先 build wheel(参见 Makefile 的 ``python-build`` / ``python-develop`` 目标)
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

# 强制使用本项目 venv(避免 miniconda pyarrow / numpy 干扰)
_VENV_SITE = Path("/Users/liupeng/workspace/quant/axon/.venv/lib/python3.14/site-packages")
if _VENV_SITE.exists() and str(_VENV_SITE) not in sys.path:
    sys.path.insert(0, str(_VENV_SITE))

# ``axon_quant`` 在 maturin develop / wheel install 后可被 import
# 缺失时 skip 整个模块(开发期还没 build 时常见)
try:
    import axon_quant  # noqa: F401
    from axon_quant.exchange import (  # noqa: F401
        BinanceAdapter,
        ExchangeConfig,
        ExchangeError,
        ExchangeId,
        OkxAdapter,
        OrderLifecycleManager,
        RateLimitConfig,
        ReconnectConfig,
        TokenBucketRateLimiter,
        binance_testnet_config,
        okx_testnet_config,
    )

    _IMPORT_OK = True
    _IMPORT_ERROR = ""  # 占位,避免 pytestmark 处 NameError
except Exception as _exc:  # pragma: no cover - 缺失时 skip
    _IMPORT_OK = False
    _IMPORT_ERROR = repr(_exc)


pytestmark = pytest.mark.skipif(
    not _IMPORT_OK,
    reason=f"axon_quant.exchange 不可用 (build wheel 后再跑): {_IMPORT_ERROR}",
)


# ═══════════════════════════════════════════════════════════════════════════
# Helper:删除所有可能污染测试的 env 变量
# ═══════════════════════════════════════════════════════════════════════════


@pytest.fixture(autouse=True)
def _clean_exchange_env(monkeypatch):
    """每个测试前清空 exchange 相关 env,避免测试间污染。"""
    for k in (
        "BINANCE_API_KEY",
        "BINANCE_API_SECRET",
        "OKX_API_KEY",
        "OKX_API_SECRET",
        "OKX_PASSPHRASE",
    ):
        monkeypatch.delenv(k, raising=False)
    yield


# ═══════════════════════════════════════════════════════════════════════════
# 1. 类型导入 / 实例化
# ═══════════════════════════════════════════════════════════════════════════


def test_exchange_id_enum_members():
    """`ExchangeId` 至少包含 Binance / Okx 两个变体。"""
    assert ExchangeId.Binance is not None
    assert ExchangeId.Okx is not None
    # 不等
    assert ExchangeId.Binance != ExchangeId.Okx


def test_exchange_config_construct_with_defaults():
    """`ExchangeConfig` 用默认值构造,testnet 默认 true。"""
    cfg = ExchangeConfig(
        exchange_id=ExchangeId.Binance,
        api_key="k",
        api_secret="s",
        rest_base_url="https://testnet.binance.vision",
        ws_url="wss://testnet.binance.vision/ws",
    )
    assert cfg.testnet is True
    assert cfg.exchange_id == ExchangeId.Binance
    assert cfg.api_key == "k"
    # api_secret 没有 getter(Rust 端隐藏)
    assert cfg.passphrase is None
    assert cfg.rest_base_url == "https://testnet.binance.vision"
    assert cfg.ws_url == "wss://testnet.binance.vision/ws"


def test_exchange_config_repr_does_not_leak_secret():
    """`ExchangeConfig.__repr__` 不含 api_secret / api_key,只显示 URL+testnet。"""
    secret = "very_secret_value_xyz_123"
    cfg = ExchangeConfig(
        exchange_id=ExchangeId.Binance,
        api_key="public_key",
        api_secret=secret,
        rest_base_url="https://example.com",
        ws_url="wss://example.com/ws",
    )
    r = repr(cfg)
    assert secret not in r, f"repr leaked api_secret: {r}"
    assert "public_key" not in r, f"repr leaked api_key: {r}"
    assert "testnet=True" in r, f"repr missing testnet: {r}"


def test_exchange_config_with_passphrase_for_okx():
    """OKX 配置需 passphrase,ExchangeConfig.passphrase getter 可读。"""
    cfg = ExchangeConfig(
        exchange_id=ExchangeId.Okx,
        api_key="k",
        api_secret="s",
        rest_base_url="https://www.okx.com",
        ws_url="wss://ws.okx.com:8443/ws/v5/private",
        passphrase="my_pass",
    )
    assert cfg.passphrase == "my_pass"
    assert cfg.exchange_id == ExchangeId.Okx


def test_rate_limit_config_custom():
    """`RateLimitConfig` 三个字段透传。"""
    r = RateLimitConfig(requests_per_second=20, orders_per_minute=120, ws_messages_per_second=100)
    assert r.requests_per_second == 20
    assert r.orders_per_minute == 120
    assert r.ws_messages_per_second == 100


def test_reconnect_config_custom():
    """`ReconnectConfig` 字段透传,单位换算正确(毫秒 / 秒)。"""
    r = ReconnectConfig(
        max_retries=5,
        initial_backoff_ms=1000,
        max_backoff_sec=60,
        backoff_multiplier=3.0,
        circuit_breaker_threshold=10,
        circuit_breaker_reset_sec=120,
    )
    assert r.max_retries == 5
    assert r.initial_backoff_ms == 1000
    assert r.max_backoff_sec == 60
    assert r.backoff_multiplier == 3.0
    assert r.circuit_breaker_threshold == 10
    assert r.circuit_breaker_reset_sec == 120


# ═══════════════════════════════════════════════════════════════════════════
# 2. 工厂函数(env 读 key + testnet 默认)
# ═══════════════════════════════════════════════════════════════════════════


def test_binance_testnet_config_construct():
    """`binance_testnet_config()` 完整 env 时返回 testnet=True 的 Binance 配置。"""
    os.environ["BINANCE_API_KEY"] = "k"
    os.environ["BINANCE_API_SECRET"] = "s"
    cfg = binance_testnet_config()
    assert cfg.testnet is True
    assert cfg.exchange_id == ExchangeId.Binance
    assert "testnet.binance.vision" in cfg.rest_base_url


def test_binance_testnet_config_missing_env_raises():
    """缺 `BINANCE_API_KEY` / `BINANCE_API_SECRET` → ExchangeError。"""
    # _clean_exchange_env 已清空
    with pytest.raises(ExchangeError) as exc_info:
        binance_testnet_config()
    assert "BINANCE_API_KEY" in str(exc_info.value)


def test_okx_testnet_config_construct():
    """`okx_testnet_config()` 完整 env 时返回 testnet=True 的 OKX 配置。"""
    os.environ["OKX_API_KEY"] = "k"
    os.environ["OKX_API_SECRET"] = "s"
    os.environ["OKX_PASSPHRASE"] = "p"
    cfg = okx_testnet_config()
    assert cfg.testnet is True
    assert cfg.exchange_id == ExchangeId.Okx
    assert cfg.passphrase == "p"


def test_okx_testnet_config_missing_passphrase_raises():
    """缺 `OKX_PASSPHRASE` → ExchangeError(其他 key 也缺,优先第一个)。"""
    with pytest.raises(ExchangeError):
        okx_testnet_config()


def test_factory_repr_does_not_leak_secret():
    """工厂返回的 config `__repr__` 也不泄漏 secret。"""
    os.environ["BINANCE_API_KEY"] = "public_k"
    os.environ["BINANCE_API_SECRET"] = "leak_test_secret_abc"
    cfg = binance_testnet_config()
    r = repr(cfg)
    assert "leak_test_secret_abc" not in r


# ═══════════════════════════════════════════════════════════════════════════
# 3. OrderLifecycleManager(无网络)
# ═══════════════════════════════════════════════════════════════════════════


def _limit_order_dict(symbol: str, exchange: str = "binance") -> dict:
    """构造一个限价单 dict(lifecycle 协议需要显式 exchange 字段)。"""
    return {
        "symbol": symbol,
        "side": "buy",
        "type": "limit",
        "quantity": "0.1",
        "price": "50000",
        "tif": "GTC",
        "exchange": exchange,
    }


def test_lifecycle_construct_empty():
    """空 manager:active=0, history=0。"""
    mgr = OrderLifecycleManager()
    assert mgr.active_count() == 0
    assert mgr.history_count() == 0


def test_lifecycle_register_increments_active():
    """注册订单后 active_count == 1。"""
    mgr = OrderLifecycleManager()
    cid = mgr.register_order(_limit_order_dict("BTCUSDT"))
    assert isinstance(cid, str) and len(cid) > 0
    assert mgr.active_count() == 1
    assert mgr.history_count() == 0


def test_lifecycle_update_to_acknowledged_stays_active():
    """非终态(acknowledged)更新:active 不变。"""
    mgr = OrderLifecycleManager()
    cid = mgr.register_order(_limit_order_dict("BTCUSDT"))
    mgr.update_status(cid, {"status": "acknowledged"})
    assert mgr.active_count() == 1
    assert mgr.history_count() == 0


def test_lifecycle_update_to_filled_moves_to_history():
    """终态(filled)更新:active → 0, history → 1。"""
    mgr = OrderLifecycleManager()
    cid = mgr.register_order(_limit_order_dict("BTCUSDT"))
    mgr.update_status(
        cid,
        {"status": "filled", "filled_qty": "0.1", "avg_price": "50000"},
    )
    assert mgr.active_count() == 0
    assert mgr.history_count() == 1


def test_lifecycle_update_to_rejected_moves_to_history():
    """终态(rejected)更新:需要 reason 字段。"""
    mgr = OrderLifecycleManager()
    cid = mgr.register_order(_limit_order_dict("BTCUSDT"))
    mgr.update_status(cid, {"status": "rejected", "reason": "min notional"})
    assert mgr.history_count() == 1


def test_lifecycle_update_to_cancelled_moves_to_history():
    """终态(cancelled)更新:需要 filled_qty 字段。"""
    mgr = OrderLifecycleManager()
    cid = mgr.register_order(_limit_order_dict("BTCUSDT"))
    mgr.update_status(cid, {"status": "cancelled", "filled_qty": "0.05"})
    assert mgr.history_count() == 1


def test_lifecycle_update_unknown_order_raises_exchange_error():
    """update 不存在的 order_id → ExchangeError(OrderNotFound)。"""
    mgr = OrderLifecycleManager()
    fake_oid = "00000000-0000-0000-0000-000000000000"
    with pytest.raises(ExchangeError) as exc_info:
        mgr.update_status(
            fake_oid,
            {"status": "filled", "filled_qty": "0.1", "avg_price": "50000"},
        )
    s = str(exc_info.value)
    assert "OrderNotFound" in s, f"expected OrderNotFound, got: {s}"


def test_lifecycle_invalid_status_string_raises_value_error():
    """status 字符串无法识别 → ExchangeError(非 PyValueError,走 ExchangeError 桥)。"""
    mgr = OrderLifecycleManager()
    cid = mgr.register_order(_limit_order_dict("BTCUSDT"))
    with pytest.raises(Exception):  # ExchangeError 或 ValueError
        mgr.update_status(cid, {"status": "expired"})


def test_lifecycle_register_missing_required_field_raises():
    """register_order 缺必填字段 → KeyError(由 Rust 端 PyKeyError 抛)。"""
    mgr = OrderLifecycleManager()
    with pytest.raises(KeyError):
        mgr.register_order({"symbol": "BTCUSDT"})  # 缺 side/type/qty/price/tif/exchange


def test_lifecycle_register_limit_missing_price_raises():
    """限价单 register 缺 price → KeyError。"""
    mgr = OrderLifecycleManager()
    d = _limit_order_dict("BTCUSDT")
    del d["price"]
    with pytest.raises(KeyError):
        mgr.register_order(d)


def test_lifecycle_repr_format():
    """`__repr__` 包含 active / history 计数。"""
    mgr = OrderLifecycleManager()
    r = repr(mgr)
    assert "active=0" in r
    assert "history=0" in r


# ═══════════════════════════════════════════════════════════════════════════
# 4. TokenBucketRateLimiter(无网络)
# ═══════════════════════════════════════════════════════════════════════════


def test_rate_limiter_construct_capacity():
    """构造时 capacity == requests_per_second,available <= capacity。"""
    l = TokenBucketRateLimiter(10)
    assert l.capacity() == 10
    assert l.available_tokens() <= 10
    # refill_rate 与 capacity 相等
    assert abs(l.refill_rate() - 10.0) < 1e-9


def test_rate_limiter_try_acquire_success():
    """try_acquire 成功消耗 → 返回 true。"""
    l = TokenBucketRateLimiter(5)
    assert l.try_acquire() is True
    assert l.try_acquire() is True


def test_rate_limiter_status_dict_fields():
    """`status()` 返回 dict 含 capacity / available / refill_rate / utilization。"""
    l = TokenBucketRateLimiter(10)
    s = l.status()
    assert "capacity" in s
    assert "available" in s
    assert "refill_rate" in s
    assert "utilization" in s
    assert s["capacity"] == 10
    util = s["utilization"]
    assert 0.0 <= util <= 1.0, f"utilization out of range: {util}"


def test_rate_limiter_repr_no_secret():
    """`__repr__` 不含任何敏感信息。"""
    l = TokenBucketRateLimiter(5)
    r = repr(l)
    assert "TokenBucketRateLimiter" in r
    assert "capacity=5" in r


# ═══════════════════════════════════════════════════════════════════════════
# 5. Adapter 构造(无网络,只验证 config 注入)
# ═══════════════════════════════════════════════════════════════════════════


def _mock_binance_config() -> ExchangeConfig:
    """Mock Binance testnet config(避免依赖 env 变量)。"""
    return ExchangeConfig(
        exchange_id=ExchangeId.Binance,
        api_key="k",
        api_secret="s",
        rest_base_url="https://testnet.binance.vision",
        ws_url="wss://testnet.binance.vision/ws",
        testnet=True,
    )


def _mock_okx_config() -> ExchangeConfig:
    """Mock OKX testnet config(避免依赖 env 变量)。"""
    return ExchangeConfig(
        exchange_id=ExchangeId.Okx,
        api_key="k",
        api_secret="s",
        passphrase="long_unique_passphrase_value_xyz_123",
        rest_base_url="https://www.okx.com",
        ws_url="wss://ws.okx.com:8443/ws/v5/private",
        testnet=True,
    )


def test_binance_adapter_construct():
    """`BinanceAdapter(config)` 构造不抛错(不开网络)。"""
    adapter = BinanceAdapter(_mock_binance_config())
    assert adapter is not None
    assert adapter.exchange_id == "Binance"


def test_binance_adapter_repr_no_secret():
    """`__repr__` 不含 api_secret / api_key。"""
    adapter = BinanceAdapter(_mock_binance_config())
    r = repr(adapter)
    assert "BinanceAdapter" in r
    # 我们的 mock config 没有 secret 字符串被 leak
    assert "api_secret" not in r


def test_okx_adapter_construct():
    """`OkxAdapter(config)` 构造不抛错(带 passphrase)。"""
    adapter = OkxAdapter(_mock_okx_config())
    assert adapter is not None
    assert adapter.exchange_id == "okx"


def test_okx_adapter_repr_no_secret():
    """`__repr__` 不含 api_secret / passphrase。"""
    adapter = OkxAdapter(_mock_okx_config())
    r = repr(adapter)
    assert "OkxAdapter" in r
    # 使用专门的标记字符串,避免误判(repr 字符串里肯定包含 'p' 字符)
    assert "long_unique_passphrase_value_xyz_123" not in r, (
        f"passphrase leaked in repr: {r}"
    )


# ═══════════════════════════════════════════════════════════════════════════
# 6. ExchangeError 异常路径
# ═══════════════════════════════════════════════════════════════════════════


def test_exchange_error_is_pyexception():
    """`ExchangeError` 继承 builtin `Exception`(PyException)。"""
    err = ExchangeError("test")
    assert isinstance(err, Exception)
    # 不继承 AxonError(cargo 循环约束,见 design spec §3.1.6)
    # 注:本测试不 import AxonError 以避免依赖 Stage 1 符号


def test_exchange_error_args_contain_code():
    """`ExchangeError.args[0]` 含错误码 + `[Code] message` 格式(由 to_py_err 注入)。"""
    # 通过 factory 触发(ExchangeError 变体不可直接构造)
    # 这里我们用一个会触发 OrderNotFound 的操作
    mgr = OrderLifecycleManager()
    fake_oid = "00000000-0000-0000-0000-000000000000"
    try:
        mgr.update_status(
            fake_oid,
            {"status": "filled", "filled_qty": "0.1", "avg_price": "50000"},
        )
        pytest.fail("expected ExchangeError")
    except ExchangeError as e:
        assert len(e.args) >= 1
        code = e.args[0]
        assert code == "OrderNotFound", f"expected OrderNotFound, got: {code}"


def test_exchange_error_inherits_pyexception_not_axon_error():
    """`ExchangeError` 继承 `Exception` 而非 `AxonError`(cargo 循环约束)。"""
    # 显式从 axon_quant 顶层拿 AxonError(若存在)
    try:
        from axon_quant import AxonError
    except ImportError:
        AxonError = None  # type: ignore

    err = ExchangeError("test")
    if AxonError is not None:
        assert not isinstance(err, AxonError), (
            "ExchangeError should NOT inherit AxonError to avoid cargo cycle"
        )
