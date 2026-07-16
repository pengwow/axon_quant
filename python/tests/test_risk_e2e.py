"""axon_quant.risk 端到端测试(L3 Python E2E)。

覆盖范围:
1. 类型导入 / 实例化
2. 工厂函数 make_order / make_portfolio / make_risk_config / make_circuit_breaker
3. DefaultRiskEngine 基础风控检查(Allow / Reject / Warn)
4. RiskReason 8 个变体扁平化 + get / get_str / to_dict
5. CircuitBreaker 触发 / 冷却 / 重置
6. update_daily_pnl + reset_daily + metrics
7. check_portfolio 返回 alerts
8. 异常路径(RiskError / KeyError / ValueError)
9. RiskMetrics 字段 + from_dict / to_dict 往返

运行::

    cd /Users/liupeng/workspace/quant/axon
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon/.venv/bin/python \\
        python -m pytest python/tests/test_risk_e2e.py -v

注意:本测试需先 build wheel(参见 Makefile 的 ``python-build`` /
``python-develop`` 目标)。如未 build,部分测试 skip。
"""

from __future__ import annotations

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
    from axon_quant.risk import (
        CircuitBreaker,
        DefaultRiskEngine,
        RiskConfig,
        RiskError,
        RiskMetrics,
        RiskReason,
        RiskResult,
        make_circuit_breaker,
        make_order,
        make_portfolio,
        make_portfolio_with_positions,
        make_risk_config,
    )
    _RISK_AVAILABLE = hasattr(axon_quant, "_native") and hasattr(
        axon_quant._native, "risk"
    )
except ImportError as _e:
    pytest.skip(f"axon_quant not installed: {_e}", allow_module_level=True)
    raise  # 实际不可达,仅供类型检查

if not _RISK_AVAILABLE:
    pytest.skip(
        "axon_quant._native.risk not yet registered (need maturin develop)",
        allow_module_level=True,
    )


# ═══════════════════════════════════════════════════════════════════════════
# 类型可用性
# ═══════════════════════════════════════════════════════════════════════════


def test_risk_module_imports_all_symbols():
    """所有 risk 顶层符号都能 import。"""
    assert DefaultRiskEngine is not None
    assert RiskConfig is not None
    assert CircuitBreaker is not None
    assert RiskMetrics is not None
    assert RiskResult is not None
    assert RiskReason is not None
    assert RiskError is not None
    # 工厂函数
    assert callable(make_order)
    assert callable(make_portfolio)
    assert callable(make_portfolio_with_positions)
    assert callable(make_risk_config)
    assert callable(make_circuit_breaker)


def test_risk_submodule_path():
    """axon_quant.risk 子模块路径可达。"""
    assert hasattr(axon_quant, "risk")
    # risk.py 模块(纯 Python wrapper)
    assert axon_quant.risk.__file__.endswith("risk.py")


# ═══════════════════════════════════════════════════════════════════════════
# 工厂函数
# ═══════════════════════════════════════════════════════════════════════════


def test_make_order_limit():
    """make_order 限价单 dict 字段齐全。"""
    o = make_order(id=1, symbol="BTC-USDT", side="Buy",
                   type="limit", price=100.0, quantity=1.0)
    assert o == {
        "id": 1,
        "symbol": "BTC-USDT",
        "side": "Buy",
        "type": "limit",
        "price": 100.0,
        "quantity": 1.0,
        "tif": "GTC",
    }


def test_make_order_market():
    """make_order 市价单无 price 字段。"""
    o = make_order(id=2, symbol="ETH-USDT", side="Sell",
                   type="market", quantity=0.5, tif="IOC")
    assert o["type"] == "market"
    assert "price" not in o
    assert o["tif"] == "IOC"


def test_make_order_limit_without_price_raises():
    """make_order 限价单缺 price → ValueError。"""
    with pytest.raises(ValueError, match="limit order requires 'price'"):
        make_order(id=1, symbol="BTC-USDT", side="Buy",
                   type="limit", quantity=1.0)


def test_make_portfolio_minimal():
    """make_portfolio 最简(只填必填字段)。"""
    p = make_portfolio(base_currency="USD", commission_rate=0.001)
    assert p == {"base_currency": "USD", "commission_rate": 0.001}


def test_make_portfolio_with_cash():
    """make_portfolio 含 cash 字段。"""
    p = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    assert p["cash"] == {"USD": 100_000.0}


def test_make_portfolio_with_positions():
    """make_portfolio_with_positions 含 cash + positions。"""
    p = make_portfolio_with_positions(
        base_currency="USD",
        cash={"USD": 50_000.0},
        positions={
            "BTC-USDT": {"quantity": 1.0, "avg_cost": 50_000.0, "market_price": 55_000.0},
        },
    )
    assert p["cash"] == {"USD": 50_000.0}
    assert "BTC-USDT" in p["positions"]
    assert p["positions"]["BTC-USDT"]["quantity"] == 1.0
    assert p["positions"]["BTC-USDT"]["market_price"] == 55_000.0


def test_make_risk_config_defaults():
    """make_risk_config 默认参数。"""
    c = make_risk_config()
    assert c.max_position_per_instrument == 100_000.0
    assert c.max_total_exposure == 1_000_000.0
    assert c.max_order_value == 50_000.0
    assert c.max_leverage == 5.0
    assert c.max_drawdown == 0.15
    assert c.max_daily_loss == 10_000.0
    assert c.max_concentration == 0.40
    assert c.circuit_breaker_cooldown_secs == 3600


def test_make_risk_config_custom():
    """make_risk_config 自定义参数。"""
    c = make_risk_config(
        max_position_per_instrument=500.0,
        max_order_value=1000.0,
        max_daily_loss=2000.0,
        circuit_breaker_cooldown_secs=60,
    )
    assert c.max_position_per_instrument == 500.0
    assert c.max_order_value == 1000.0
    assert c.max_daily_loss == 2000.0
    assert c.circuit_breaker_cooldown_secs == 60


def test_make_circuit_breaker_defaults():
    """make_circuit_breaker 默认参数。"""
    cb = make_circuit_breaker()
    assert cb.daily_loss_limit == 10_000.0
    assert cb.cooldown_seconds == 3600
    assert cb.is_active is False


# ═══════════════════════════════════════════════════════════════════════════
# DefaultRiskEngine 基础
# ═══════════════════════════════════════════════════════════════════════════


def test_engine_construct():
    """DefaultRiskEngine 构造。"""
    engine = DefaultRiskEngine(make_risk_config(max_order_value=1000.0))
    assert repr(engine).startswith("DefaultRiskEngine")


def test_engine_check_order_valid_returns_allow():
    """合法订单 → Allow。"""
    engine = DefaultRiskEngine(make_risk_config(max_order_value=10_000.0))
    order = make_order(id=1, symbol="BTC-USDT", side="Buy",
                       type="limit", price=100.0, quantity=1.0)
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    r = engine.check_order(order, portfolio)
    assert r.is_allow is True
    assert r.is_reject is False
    assert r.kind == "Allow"
    assert r.to_dict()["kind"] == "Allow"


def test_engine_check_order_oversized_returns_reject():
    """超大订单 → Reject(OrderTooLarge)。"""
    engine = DefaultRiskEngine(make_risk_config(max_order_value=1000.0))
    order = make_order(id=1, symbol="BTC-USDT", side="Buy",
                       type="limit", price=100.0, quantity=20.0)  # 2000 > 1000
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    r = engine.check_order(order, portfolio)
    assert r.is_reject is True
    assert r.kind == "Reject"
    reason = r.reason
    assert reason is not None
    assert reason.kind == "OrderTooLarge"
    d = reason.to_dict()
    assert d["max"] == 1000.0
    assert d["actual"] == 2000.0


def test_engine_check_order_market():
    """市价单不需要 price。"""
    engine = DefaultRiskEngine(make_risk_config(max_order_value=10_000.0))
    order = make_order(id=1, symbol="ETH-USDT", side="Sell",
                       type="market", quantity=0.5, tif="IOC")
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    r = engine.check_order(order, portfolio)
    assert r.is_allow is True


# ═══════════════════════════════════════════════════════════════════════════
# RiskResult / RiskReason 工厂
# ═══════════════════════════════════════════════════════════════════════════


def test_risk_result_allow_factory():
    """RiskResult.allow() 工厂。"""
    r = RiskResult.allow()
    assert r.is_allow
    assert not r.is_reject
    assert not r.is_warn
    assert r.reason is None
    assert r.message is None
    assert r.kind == "Allow"


def test_risk_result_reject_factory():
    """RiskResult.reject(reason) 工厂。"""
    reason = RiskReason.from_dict(
        {"kind": "OrderTooLarge", "max": 1000.0, "actual": 2000.0}
    )
    r = RiskResult.reject(reason)
    assert r.is_reject
    assert r.kind == "Reject"
    assert r.reason is not None
    assert r.reason.kind == "OrderTooLarge"


def test_risk_result_warn_factory():
    """RiskResult.warn(message) 工厂。"""
    r = RiskResult.warn("leverage approaching limit")
    assert r.is_warn
    assert r.kind == "Warn"
    assert r.message == "leverage approaching limit"


def test_risk_reason_to_dict():
    """RiskReason.to_dict() 含所有 fields。"""
    r = RiskReason.from_dict(
        {"kind": "PositionLimitExceeded", "limit": 1000.0, "instrument": "BTC-USDT"}
    )
    d = r.to_dict()
    assert d["kind"] == "PositionLimitExceeded"
    assert d["limit"] == 1000.0
    assert d["instrument"] == "BTC-USDT"


def test_risk_reason_get_and_get_str():
    """RiskReason.get / get_str 字段访问。"""
    r = RiskReason.from_dict(
        {"kind": "ConcentrationTooHigh", "pct": 0.5, "instrument": "ETH-USDT"}
    )
    assert r.get("pct") == 0.5
    assert r.get("nonexistent") is None
    assert r.get_str("instrument") == "ETH-USDT"
    assert r.get_str("nonexistent") is None


# ═══════════════════════════════════════════════════════════════════════════
# CircuitBreaker
# ═══════════════════════════════════════════════════════════════════════════


def test_circuit_breaker_initial_inactive():
    """CircuitBreaker 初始未激活。"""
    cb = make_circuit_breaker(daily_loss_limit=1000.0, cooldown_seconds=3600)
    assert cb.is_active is False
    assert cb.daily_loss_limit == 1000.0
    assert cb.cooldown_seconds == 3600


def test_circuit_breaker_triggers_on_loss():
    """亏损达到阈值触发熔断。"""
    cb = make_circuit_breaker(daily_loss_limit=1000.0, cooldown_seconds=3600)
    cb.check_and_trigger(-500.0)
    assert cb.is_active is False  # 不到阈值
    cb.check_and_trigger(-1500.0)  # 触发
    assert cb.is_active is True


def test_circuit_breaker_reset():
    """reset() 强制解除熔断。"""
    cb = make_circuit_breaker(daily_loss_limit=1000.0, cooldown_seconds=3600)
    cb.check_and_trigger(-1500.0)
    assert cb.is_active is True
    cb.reset()
    assert cb.is_active is False


# ═══════════════════════════════════════════════════════════════════════════
# update_daily_pnl + reset_daily + metrics
# ═══════════════════════════════════════════════════════════════════════════


def test_update_daily_pnl_updates_metrics():
    """update_daily_pnl 累加 + metrics 读出。"""
    engine = DefaultRiskEngine(make_risk_config())
    engine.update_daily_pnl(500.0)
    engine.update_daily_pnl(-200.0)
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    m = engine.metrics(portfolio)
    assert m["daily_realized_pnl"] == pytest.approx(300.0)


def test_reset_daily_clears_pnl_and_breaker():
    """reset_daily 重置日内 PnL + 熔断器。"""
    engine = DefaultRiskEngine(make_risk_config(max_daily_loss=1000.0))
    engine.update_daily_pnl(-1500.0)  # 触发熔断
    engine.reset_daily()
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    m = engine.metrics(portfolio)
    assert m["daily_realized_pnl"] == 0.0
    # 重置后,订单可被允许
    order = make_order(id=1, symbol="BTC-USDT", side="Buy",
                       type="limit", price=100.0, quantity=1.0)
    r = engine.check_order(order, portfolio)
    assert r.is_allow is True


def test_circuit_breaker_via_engine_blocks_order():
    """engine.update_daily_pnl 触发熔断后,check_order 拒绝。"""
    engine = DefaultRiskEngine(make_risk_config(max_daily_loss=1000.0))
    engine.update_daily_pnl(-1500.0)
    order = make_order(id=1, symbol="BTC-USDT", side="Buy",
                       type="limit", price=100.0, quantity=1.0)
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    r = engine.check_order(order, portfolio)
    assert r.is_reject is True
    assert r.reason.kind == "CircuitBreakerActive"


# ═══════════════════════════════════════════════════════════════════════════
# check_portfolio / alerts
# ═══════════════════════════════════════════════════════════════════════════


def test_check_portfolio_returns_alerts_on_daily_loss():
    """日内亏损超阈值 → check_portfolio 返回 alerts。"""
    engine = DefaultRiskEngine(make_risk_config(max_daily_loss=1000.0))
    engine.update_daily_pnl(-2000.0)
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 10_000.0})
    alerts = engine.check_portfolio(portfolio)
    assert isinstance(alerts, list)
    assert len(alerts) >= 1
    # 每个 alert 是 dict
    assert isinstance(alerts[0], dict)
    assert "severity" in alerts[0]
    assert "reason" in alerts[0]


# ═══════════════════════════════════════════════════════════════════════════
# RiskMetrics 独立类
# ═══════════════════════════════════════════════════════════════════════════


def test_risk_metrics_default_constructor():
    """RiskMetrics 默认构造(全 0)。"""
    m = RiskMetrics()
    assert m.total_exposure == 0.0
    assert m.leverage == 0.0
    assert m.current_drawdown == 0.0
    assert m.daily_realized_pnl == 0.0
    assert m.var_95 == 0.0
    assert m.concentration == {}


def test_risk_metrics_custom_constructor():
    """RiskMetrics 自定义参数。"""
    m = RiskMetrics(
        total_exposure=100_000.0,
        leverage=2.5,
        current_drawdown=0.05,
        daily_realized_pnl=500.0,
        var_95=1_000.0,
        concentration={"BTC-USDT": 0.45},
    )
    assert m.total_exposure == 100_000.0
    assert m.leverage == 2.5
    assert m.concentration["BTC-USDT"] == 0.45


def test_risk_metrics_to_dict():
    """RiskMetrics.to_dict() 字段完整。"""
    m = RiskMetrics(
        total_exposure=50_000.0,
        leverage=1.5,
        current_drawdown=0.10,
        daily_realized_pnl=1_000.0,
        var_95=2_000.0,
        concentration={"BTC-USDT": 0.6},
    )
    d = m.to_dict()
    assert d["total_exposure"] == 50_000.0
    assert d["leverage"] == 1.5
    assert d["current_drawdown"] == 0.10
    assert d["daily_realized_pnl"] == 1_000.0
    assert d["var_95"] == 2_000.0
    assert d["concentration"]["BTC-USDT"] == 0.6


def test_risk_metrics_from_dict_roundtrip():
    """RiskMetrics.from_dict() 构造。"""
    d = {
        "total_exposure": 50_000.0,
        "leverage": 1.5,
        "current_drawdown": 0.10,
        "daily_realized_pnl": 1_000.0,
        "var_95": 2_000.0,
        "concentration": {"BTC-USDT": 0.5},
    }
    m = RiskMetrics.from_dict(d)
    assert m.total_exposure == 50_000.0
    assert m.leverage == 1.5
    assert m.concentration["BTC-USDT"] == 0.5


def test_risk_metrics_from_dict_missing_field_raises():
    """RiskMetrics.from_dict() 缺字段 → KeyError。"""
    d = {"total_exposure": 50_000.0}
    with pytest.raises(KeyError):
        RiskMetrics.from_dict(d)


# ═══════════════════════════════════════════════════════════════════════════
# 异常路径
# ═══════════════════════════════════════════════════════════════════════════


def test_engine_check_order_missing_field_raises_keyerror():
    """check_order 缺字段 → KeyError。"""
    engine = DefaultRiskEngine(make_risk_config())
    # order 缺 quantity 字段
    order = {
        "id": 1,
        "symbol": "BTC-USDT",
        "side": "Buy",
        "type": "limit",
        "price": 100.0,
        "tif": "GTC",
    }
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    with pytest.raises(KeyError):
        engine.check_order(order, portfolio)


def test_engine_check_order_invalid_side_raises_valueerror():
    """check_order 非法 side 字符串 → ValueError。"""
    engine = DefaultRiskEngine(make_risk_config())
    order = {
        "id": 1,
        "symbol": "BTC-USDT",
        "side": "XXX",
        "type": "market",
        "quantity": 1.0,
        "tif": "GTC",
    }
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    with pytest.raises(ValueError, match="invalid side"):
        engine.check_order(order, portfolio)


def test_engine_check_order_unsupported_type_raises_valueerror():
    """check_order 不支持的 order type(stop) → ValueError。"""
    engine = DefaultRiskEngine(make_risk_config())
    order = {
        "id": 1,
        "symbol": "BTC-USDT",
        "side": "Buy",
        "type": "stop",
        "price": 100.0,
        "quantity": 1.0,
        "tif": "GTC",
    }
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    with pytest.raises(ValueError, match="unsupported order type"):
        engine.check_order(order, portfolio)


def test_risk_error_inherits_exception():
    """RiskError 继承 Exception(可被 ``except Exception`` 捕获)。"""
    # 注:RiskError 继承 builtin PyException,而非 AxonError(避免 cargo 循环)
    assert issubclass(RiskError, Exception)


# ═══════════════════════════════════════════════════════════════════════
# 无参构造 → UserWarning 行为(0.4.1 新增)
# ═══════════════════════════════════════════════════════════════════════


def test_engine_no_args_emits_user_warning():
    """`DefaultRiskEngine()` 无参构造 → emit UserWarning 提示用了宽松默认。"""
    import warnings as _warnings

    with _warnings.catch_warnings(record=True) as caught:
        _warnings.simplefilter("always")
        engine = DefaultRiskEngine()  # noqa: F841  — 仅触发 warning
    # 应当恰好触发 1 条 UserWarning
    user_warnings = [w for w in caught if issubclass(w.category, UserWarning)]
    assert len(user_warnings) == 1, (
        f"expected 1 UserWarning, got {len(user_warnings)}: "
        f"{[str(w.message) for w in caught]}"
    )
    msg = str(user_warnings[0].message)
    # 提示信息应包含关键提醒字段
    assert "default RiskConfig" in msg
    assert "lenient" in msg or "production" in msg


def test_engine_explicit_config_no_warning():
    """显式传 `RiskConfig` → 不触发任何 UserWarning。"""
    import warnings as _warnings

    cfg = make_risk_config(max_order_value=10_000.0, max_leverage=2.0)
    with _warnings.catch_warnings(record=True) as caught:
        _warnings.simplefilter("always")
        engine = DefaultRiskEngine(cfg)  # noqa: F841
    user_warnings = [w for w in caught if issubclass(w.category, UserWarning)]
    assert user_warnings == [], (
        f"expected 0 UserWarning with explicit config, got: "
        f"{[str(w.message) for w in user_warnings]}"
    )


def test_engine_explicit_none_same_as_no_args():
    """`DefaultRiskEngine(None)` 与 `DefaultRiskEngine()` 等价:都触发 warning。"""
    import warnings as _warnings

    with _warnings.catch_warnings(record=True) as caught:
        _warnings.simplefilter("always")
        engine = DefaultRiskEngine(None)  # noqa: F841
    user_warnings = [w for w in caught if issubclass(w.category, UserWarning)]
    assert len(user_warnings) == 1


def test_engine_warning_can_be_filtered():
    """用 `warnings.filterwarnings('ignore')` 静默 → 构造仍成功。"""
    import warnings as _warnings

    with _warnings.catch_warnings():
        _warnings.filterwarnings("ignore", category=UserWarning)
        # 静默模式下应不抛错
        engine = DefaultRiskEngine()
    # 引擎可正常用(说明无参路径也完整构造)
    portfolio = make_portfolio(base_currency="USD", cash={"USD": 100_000.0})
    order = make_order(id=1, symbol="BTC-USDT", side="Buy",
                       type="limit", price=100.0, quantity=1.0)
    result = engine.check_order(order, portfolio)
    # 默认 max_order_value=50_000, 单笔 100 < 阈值, 应当 Allow
    assert result.is_allow(), f"expected Allow with default config, got: {result}"
