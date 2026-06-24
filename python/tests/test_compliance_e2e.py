"""axon_quant.compliance 端到端测试(L3 Python E2E)。

覆盖范围:
1. 类型导入 / 实例化(ComplianceModule / ComplianceConfig / 5 个枚举 / TradeRecord)
2. 枚举 __str__ / from_str(TradeSide / OrderType / LiquidityType / TradeStatus)
3. AuditEventType 枚举 __str__
4. ComplianceConfig 字段 getter(account_id / base_currency / regulators 等)
5. TradeRecord.required_fields / optional_fields 元信息
6. ComplianceModule 构造(ComplianceConfig + storage_path)
7. ComplianceModule 构造(从 TOML 路径 + storage_path)load_config_from_toml
8. record_trade(dict 协议)基本流程
9. record_trade 可选字段(order_type / liquidity / status / realized_pnl)
10. record_trade 缺必填字段抛 KeyError
11. record_trade side / order_type / liquidity 字符串大小写不敏感
12. record_trade 数量 <= 0 / 价格 <= 0 抛 ComplianceError(InvalidTradeData)
13. trade_count / audit_event_count getter 自增
14. verify_audit_integrity() 返回 True
15. query_trades({}) 返回全部交易
16. query_trades(symbol=...) 过滤
17. query_trades(side=...) 过滤
18. query_trades(strategy_id=...) 过滤
19. query_trades(min_notional=...) 过滤
20. query_trades(start_time / end_time RFC3339) 过滤
21. get_trade_stats(start, end) 返回 dict
22. generate_daily_report(date, starting_balance) 返回 dict
23. generate_monthly_report(year, month) 返回 dict
24. generate_annual_report(year, initial_balance) 返回 dict
25. __repr__ 包含 account_id / base_currency
26. ComplianceError 异常路径(无效 side / 缺字段 / 负 quantity)
27. 同一 storage 路径可重新打开并加载历史 trade(由 verify_audit_integrity 保证)

运行::

    cd /Users/liupeng/workspace/quant/axon
    PYO3_PYTHON=/Users/liupeng/workspace/quant/axon/.venv/bin/python \\
        python -m pytest python/tests/test_compliance_e2e.py -v

注意:本测试需先 build wheel(参见 Makefile 的 ``python-build`` /
``python-develop`` 目标)。如未 build,部分测试 skip。
"""

from __future__ import annotations

import datetime
import os
import sys
import tempfile
import uuid
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
    from axon_quant.compliance import (  # noqa: F401
        AuditEventType,
        ComplianceConfig,
        ComplianceError,
        ComplianceModule,
        LiquidityType,
        OrderType,
        TradeRecord,
        TradeSide,
        TradeStatus,
        load_config_from_toml,
    )
    _COMPLIANCE_AVAILABLE = hasattr(axon_quant, "_native") and hasattr(
        axon_quant._native, "compliance"
    )
except ImportError as _e:
    pytest.skip(f"axon_quant not installed: {_e}", allow_module_level=True)
    raise  # 实际不可达,仅供类型检查

if not _COMPLIANCE_AVAILABLE:
    pytest.skip(
        "axon_quant._native.compliance not yet registered (need maturin develop)",
        allow_module_level=True,
    )


# ═══════════════════════════════════════════════════════════════════════════
# Fixtures
# ═══════════════════════════════════════════════════════════════════════════


@pytest.fixture
def tmp_storage() -> str:
    """每个测试独享的临时存储目录,test 结束自动清理。"""
    d = tempfile.mkdtemp(prefix="axon_compliance_test_")
    yield d
    # 不显式清理:tempfile.mkdtemp 创建的目录留给 OS 回收即可


@pytest.fixture
def test_config() -> ComplianceConfig:
    """标准测试配置。"""
    return ComplianceConfig(
        account_id="acc-test",
        base_currency="USDT",
        large_trade_threshold=100_000.0,
        position_limit=1_000_000.0,
        max_portfolio_concentration=0.4,
        data_retention_years=7,
        regulators=["SEC", "FINRA"],
    )


@pytest.fixture
def compliance(test_config, tmp_storage) -> ComplianceModule:
    """标准合规模块实例。"""
    return ComplianceModule(test_config, tmp_storage)


def _now_rfc3339() -> str:
    """返回当前 UTC 时间的 RFC3339 字符串。"""
    return (
        datetime.datetime.now(datetime.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def _sample_trade(**overrides) -> dict:
    """构造一笔标准 trade dict(可覆盖任意字段)。"""
    base = {
        "strategy_id": "strat-1",
        "symbol": "BTCUSDT",
        "side": "buy",
        "quantity": 1.0,
        "price": 50_000.0,
        "fee": 50.0,
        "fee_currency": "USDT",
        "exchange": "Binance",
        "execution_time": _now_rfc3339(),
    }
    base.update(overrides)
    return base


# ═══════════════════════════════════════════════════════════════════════════
# 类型可用性
# ═══════════════════════════════════════════════════════════════════════════


def test_compliance_module_imports_all_symbols():
    """所有 compliance 顶层符号都能 import。"""
    assert ComplianceModule is not None
    assert ComplianceConfig is not None
    assert TradeSide is not None
    assert OrderType is not None
    assert LiquidityType is not None
    assert TradeStatus is not None
    assert AuditEventType is not None
    assert TradeRecord is not None
    assert ComplianceError is not None
    assert load_config_from_toml is not None


def test_compliance_native_submodule_accessible():
    """`_native.compliance` 子模块可访问,包含与顶层同名符号。"""
    native_compliance = axon_quant._native.compliance
    assert native_compliance.ComplianceModule is ComplianceModule
    assert native_compliance.ComplianceConfig is ComplianceConfig
    assert native_compliance.ComplianceError is ComplianceError


# ═══════════════════════════════════════════════════════════════════════════
# 枚举类型
# ═══════════════════════════════════════════════════════════════════════════


def test_trade_side_values():
    """TradeSide 有 Buy / Sell 两个变体。"""
    assert TradeSide.Buy is not None
    assert TradeSide.Sell is not None


def test_trade_side_str():
    """TradeSide.__str__ 返回小写字符串。"""
    assert str(TradeSide.Buy) == "buy"
    assert str(TradeSide.Sell) == "sell"


def test_trade_side_from_str_case_insensitive():
    """TradeSide.from_str 接受大小写不敏感字符串。"""
    assert TradeSide.from_str("buy") == TradeSide.Buy
    assert TradeSide.from_str("BUY") == TradeSide.Buy
    assert TradeSide.from_str("Sell") == TradeSide.Sell
    with pytest.raises(ValueError):
        TradeSide.from_str("hold")


def test_order_type_values():
    """OrderType 有 6 个变体。"""
    for v in ("Market", "Limit", "StopLoss", "TakeProfit", "StopLimit", "TrailingStop"):
        assert getattr(OrderType, v) is not None


def test_order_type_str():
    """OrderType.__str__ 返回 snake_case 字符串。"""
    assert str(OrderType.Market) == "market"
    assert str(OrderType.Limit) == "limit"
    assert str(OrderType.StopLoss) == "stop_loss"
    assert str(OrderType.TakeProfit) == "take_profit"
    assert str(OrderType.TrailingStop) == "trailing_stop"


def test_order_type_from_str_accepts_aliases():
    """OrderType.from_str 同时接受 snake_case 和 camelCase。"""
    assert OrderType.from_str("market") == OrderType.Market
    assert OrderType.from_str("LIMIT") == OrderType.Limit
    assert OrderType.from_str("stop_loss") == OrderType.StopLoss
    assert OrderType.from_str("stoploss") == OrderType.StopLoss
    assert OrderType.from_str("trailingstop") == OrderType.TrailingStop
    with pytest.raises(ValueError):
        OrderType.from_str("foo")


def test_liquidity_type_values_and_str():
    """LiquidityType 有 Maker / Taker 两个变体,__str__ 返回小写。"""
    assert LiquidityType.Maker is not None
    assert LiquidityType.Taker is not None
    assert str(LiquidityType.Maker) == "maker"
    assert str(LiquidityType.Taker) == "taker"


def test_trade_status_values_and_str():
    """TradeStatus 有 5 个变体,__str__ 返回小写 snake_case。"""
    for v in ("Pending", "Filled", "PartiallyFilled", "Cancelled", "Rejected"):
        assert getattr(TradeStatus, v) is not None
    assert str(TradeStatus.Pending) == "pending"
    assert str(TradeStatus.Filled) == "filled"
    assert str(TradeStatus.PartiallyFilled) == "partially_filled"
    assert str(TradeStatus.Cancelled) == "cancelled"
    assert str(TradeStatus.Rejected) == "rejected"


def test_audit_event_type_str():
    """AuditEventType 至少包含 trade_executed / order_placed / position_opened 等。"""
    assert str(AuditEventType.TradeExecuted) == "trade_executed"
    assert str(AuditEventType.OrderPlaced) == "order_placed"
    assert str(AuditEventType.OrderCancelled) == "order_cancelled"
    assert str(AuditEventType.PositionOpened) == "position_opened"
    assert str(AuditEventType.PositionClosed) == "position_closed"
    assert str(AuditEventType.StrategyStarted) == "strategy_started"
    assert str(AuditEventType.SystemError) == "system_error"
    assert str(AuditEventType.ComplianceAlert) == "compliance_alert"


# ═══════════════════════════════════════════════════════════════════════════
# ComplianceConfig
# ═══════════════════════════════════════════════════════════════════════════


def test_compliance_config_getters(test_config):
    """ComplianceConfig 所有字段 getter 返回正确值。"""
    assert test_config.account_id == "acc-test"
    assert test_config.base_currency == "USDT"
    assert test_config.large_trade_threshold == 100_000.0
    assert test_config.position_limit == 1_000_000.0
    assert test_config.max_portfolio_concentration == 0.4
    assert test_config.data_retention_years == 7
    assert test_config.regulators == ["SEC", "FINRA"]


def test_compliance_config_repr(test_config):
    """ComplianceConfig.__repr__ 包含 account_id。"""
    r = repr(test_config)
    assert "acc-test" in r
    assert "USDT" in r


# ═══════════════════════════════════════════════════════════════════════════
# TradeRecord
# ═══════════════════════════════════════════════════════════════════════════


def test_trade_record_required_fields():
    """TradeRecord.required_fields 包含全部 8 个必填字段。"""
    required = TradeRecord.required_fields()
    for f in [
        "strategy_id",
        "symbol",
        "side",
        "quantity",
        "price",
        "fee",
        "fee_currency",
        "exchange",
    ]:
        assert f in required, f"missing required field {f!r}"


def test_trade_record_optional_fields():
    """TradeRecord.optional_fields 包含 11 个可选字段。"""
    optional = TradeRecord.optional_fields()
    assert "status" in optional
    assert "order_type" in optional
    assert "liquidity" in optional
    assert "realized_pnl" in optional
    assert "execution_time" in optional
    assert "trade_id" in optional


# ═══════════════════════════════════════════════════════════════════════════
# ComplianceModule:基础
# ═══════════════════════════════════════════════════════════════════════════


def test_compliance_module_creation(compliance, test_config, tmp_storage):
    """新建模块初始 trade_count / audit_event_count = 0,storage 路径正确。"""
    assert compliance.trade_count == 0
    assert compliance.audit_event_count == 0
    assert compliance.storage_path == tmp_storage
    assert compliance.config.account_id == test_config.account_id
    assert compliance.verify_audit_integrity() is True


def test_compliance_module_repr(compliance):
    """__repr__ 包含 account_id / base_currency / storage_path。"""
    r = repr(compliance)
    assert "acc-test" in r
    assert "USDT" in r
    assert "storage_path" in r


def test_compliance_module_requires_storage_path_with_config(test_config):
    """传 ComplianceConfig 必须同时给 storage_path(否则 ValueError)。"""
    with pytest.raises(ValueError, match="storage_path"):
        ComplianceModule(test_config, None)  # type: ignore[arg-type]


# ═══════════════════════════════════════════════════════════════════════════
# record_trade
# ═══════════════════════════════════════════════════════════════════════════


def test_record_trade_basic(compliance):
    """record_trade(dict) 成功,trade_count / audit_event_count 各 +1。"""
    n_before = compliance.trade_count
    e_before = compliance.audit_event_count
    compliance.record_trade(_sample_trade())
    assert compliance.trade_count == n_before + 1
    assert compliance.audit_event_count == e_before + 1
    assert compliance.verify_audit_integrity() is True


def test_record_trade_with_optional_fields(compliance):
    """record_trade 接受 order_type / liquidity / status / realized_pnl。"""
    compliance.record_trade(
        _sample_trade(
            order_type="limit",
            liquidity="maker",
            status="filled",
            realized_pnl=100.0,
            funding_rate=0.0001,
            slippage=0.5,
        )
    )
    trades = compliance.query_trades({})
    assert len(trades) == 1
    t = trades[0]
    assert t["order_type"] == "Limit"
    assert t["liquidity"] == "Maker"
    assert t["status"] == "Filled"
    assert t["realized_pnl"] == 100.0


def test_record_trade_with_explicit_uuids(compliance):
    """record_trade 接受显式 trade_id / order_id (UUID 字符串)。"""
    trade_id = str(uuid.uuid4())
    order_id = str(uuid.uuid4())
    compliance.record_trade(_sample_trade(trade_id=trade_id, order_id=order_id))
    trades = compliance.query_trades({})
    assert trades[0]["trade_id"] == trade_id
    assert trades[0]["order_id"] == order_id


def test_record_trade_case_insensitive_enums(compliance):
    """record_trade 中 side / order_type / liquidity / status 字符串大小写不敏感。"""
    compliance.record_trade(
        _sample_trade(
            side="BUY",
            order_type="LIMIT",
            liquidity="MAKER",
            status="Filled",
        )
    )
    trades = compliance.query_trades({})
    assert trades[0]["side"] == "Buy"
    assert trades[0]["order_type"] == "Limit"
    assert trades[0]["liquidity"] == "Maker"
    assert trades[0]["status"] == "Filled"


def test_record_trade_missing_required_field_raises(compliance):
    """record_trade 缺必填字段抛 KeyError(且 message 包含字段名)。"""
    with pytest.raises(KeyError, match="strategy_id"):
        compliance.record_trade(
            {
                "symbol": "BTCUSDT",
                "side": "buy",
                "quantity": 1.0,
                "price": 50_000.0,
                "fee": 50.0,
                "fee_currency": "USDT",
                "exchange": "Binance",
            }
        )


def test_record_trade_invalid_side_raises(compliance):
    """side 字符串无效抛 ValueError。"""
    with pytest.raises(ValueError, match="side"):
        compliance.record_trade(_sample_trade(side="hold"))


def test_record_trade_negative_quantity_raises_compliance_error(compliance):
    """quantity <= 0 抛 ComplianceError(InvalidTradeData),trade_count 不变。"""
    n_before = compliance.trade_count
    with pytest.raises(ComplianceError) as excinfo:
        compliance.record_trade(_sample_trade(quantity=-1.0))
    # to_py_err 把错误码包在 args[0]
    assert "InvalidTradeData" in str(excinfo.value)
    assert compliance.trade_count == n_before


def test_record_trade_negative_price_raises_compliance_error(compliance):
    """price <= 0 抛 ComplianceError(InvalidTradeData)。"""
    with pytest.raises(ComplianceError) as excinfo:
        compliance.record_trade(_sample_trade(price=-1.0, quantity=1.0))
    assert "InvalidTradeData" in str(excinfo.value)


def test_record_trade_wrong_field_type_raises_value_error(compliance):
    """字段类型错(price 传字符串)抛 ValueError。"""
    with pytest.raises(ValueError):
        compliance.record_trade(_sample_trade(price="not-a-number"))


# ═══════════════════════════════════════════════════════════════════════════
# query_trades
# ═══════════════════════════════════════════════════════════════════════════


def _seed_multi_trades(compliance: ComplianceModule) -> None:
    """写入 4 笔多样化 trade 用于过滤测试。"""
    compliance.record_trade(_sample_trade(symbol="BTCUSDT", side="buy", quantity=1.0, price=50_000.0))
    compliance.record_trade(
        _sample_trade(symbol="ETHUSDT", side="sell", quantity=10.0, price=3_000.0)
    )
    compliance.record_trade(
        _sample_trade(
            symbol="BTCUSDT",
            side="sell",
            quantity=0.5,
            price=51_000.0,
            strategy_id="strat-2",
        )
    )
    compliance.record_trade(
        _sample_trade(
            symbol="BNBUSDT",
            side="buy",
            quantity=100.0,
            price=300.0,
            realized_pnl=10.0,
        )
    )


def test_query_trades_no_filter_returns_all(compliance):
    """query_trades({}) 返回全部 trade。"""
    _seed_multi_trades(compliance)
    trades = compliance.query_trades({})
    assert len(trades) == 4


def test_query_trades_by_symbol(compliance):
    """query_trades(symbol='BTCUSDT') 只返回 BTCUSDT。"""
    _seed_multi_trades(compliance)
    trades = compliance.query_trades({"symbol": "BTCUSDT"})
    assert len(trades) == 2
    assert all(t["symbol"] == "BTCUSDT" for t in trades)


def test_query_trades_by_side(compliance):
    """query_trades(side='buy') 只返回 buy 方向。"""
    _seed_multi_trades(compliance)
    trades = compliance.query_trades({"side": "buy"})
    assert len(trades) == 2
    assert all(t["side"] == "Buy" for t in trades)


def test_query_trades_by_strategy_id(compliance):
    """query_trades(strategy_id='strat-2') 只返回该策略。"""
    _seed_multi_trades(compliance)
    trades = compliance.query_trades({"strategy_id": "strat-2"})
    assert len(trades) == 1
    assert trades[0]["strategy_id"] == "strat-2"


def test_query_trades_by_min_notional(compliance):
    """query_trades(min_notional=10_000) 过滤小额 trade。"""
    _seed_multi_trades(compliance)
    # BNB 单笔 30_000;ETH 30_000;BTC 50_000 / 25_500 → 4 笔均 >= 25_000
    # min_notional=40_000 应只剩 2 笔 BTC
    trades = compliance.query_trades({"min_notional": 40_000.0})
    assert all(t["notional_value"] >= 40_000.0 for t in trades)
    assert len(trades) >= 1


def test_query_trades_by_time_range(compliance):
    """query_trades(start_time / end_time RFC3339) 按时间过滤。"""
    _seed_multi_trades(compliance)
    trades = compliance.query_trades(
        {
            "start_time": "2000-01-01T00:00:00Z",
            "end_time": "2030-01-01T00:00:00Z",
        }
    )
    assert len(trades) == 4


def test_query_trades_invalid_time_raises(compliance):
    """start_time 不是合法 RFC3339 抛 ValueError。"""
    _seed_multi_trades(compliance)
    with pytest.raises(ValueError):
        compliance.query_trades({"start_time": "not-a-date"})


# ═══════════════════════════════════════════════════════════════════════════
# get_trade_stats / 报告生成
# ═══════════════════════════════════════════════════════════════════════════


def test_get_trade_stats_basic(compliance):
    """get_trade_stats(start, end) 返回 dict,字段齐全。"""
    _seed_multi_trades(compliance)
    stats = compliance.get_trade_stats("2000-01-01T00:00:00Z", "2030-01-01T00:00:00Z")
    assert isinstance(stats, dict)
    assert stats["total_trades"] == 4
    # total_volume = 50000 + 30000 + 25500 + 30000 = 135500
    assert stats["total_volume"] == pytest.approx(135_500.0, rel=1e-6)
    assert "total_fees" in stats
    assert "win_rate" in stats
    assert "avg_trade_size" in stats


def test_get_trade_stats_winning_losing(compliance):
    """winning_trades / losing_trades 按 realized_pnl 正负归类。"""
    compliance.record_trade(_sample_trade(realized_pnl=100.0))
    compliance.record_trade(_sample_trade(realized_pnl=-50.0, symbol="ETHUSDT"))
    compliance.record_trade(_sample_trade(realized_pnl=0.0, symbol="BNBUSDT"))
    stats = compliance.get_trade_stats("2000-01-01T00:00:00Z", "2030-01-01T00:00:00Z")
    assert stats["winning_trades"] == 1
    assert stats["losing_trades"] == 1
    assert stats["win_rate"] == pytest.approx(1.0 / 3.0, rel=1e-6)


def test_get_trade_stats_invalid_time_raises(compliance):
    """get_trade_stats 时间格式错抛 ValueError。"""
    with pytest.raises(ValueError):
        compliance.get_trade_stats("not-a-date", "2030-01-01T00:00:00Z")


def test_generate_daily_report(compliance):
    """generate_daily_report(date, balance) 返回 dict,字段齐全。"""
    compliance.record_trade(_sample_trade())
    report = compliance.generate_daily_report("2026-06-24", 100_000.0)
    assert isinstance(report, dict)
    # DailyReport 必有字段
    for key in ("date", "account_id", "starting_balance", "ending_balance", "net_pnl"):
        assert key in report, f"missing key {key!r} in daily report"
    assert report["account_id"] == "acc-test"


def test_generate_daily_report_invalid_date(compliance):
    """generate_daily_report 日期格式错抛 ValueError。"""
    with pytest.raises(ValueError):
        compliance.generate_daily_report("24/06/2026", 100_000.0)


def test_generate_monthly_report(compliance):
    """generate_monthly_report(year, month) 返回 dict。"""
    compliance.record_trade(_sample_trade())
    report = compliance.generate_monthly_report(2026, 6)
    assert isinstance(report, dict)
    assert "account_id" in report


def test_generate_annual_report(compliance):
    """generate_annual_report(year, balance) 返回 dict。"""
    compliance.record_trade(_sample_trade())
    report = compliance.generate_annual_report(2026, 100_000.0)
    assert isinstance(report, dict)
    assert "account_id" in report


# ═══════════════════════════════════════════════════════════════════════════
# load_config_from_toml
# ═══════════════════════════════════════════════════════════════════════════


def test_load_config_from_toml(tmp_storage):
    """从 TOML 配置文件一步创建模块,字段正确。"""
    config_path = os.path.join(tmp_storage, "test_config.toml")
    with open(config_path, "w") as f:
        f.write(
            'account_id = "acc-toml"\n'
            'base_currency = "USD"\n'
            "large_trade_threshold = 200000.0\n"
            "position_limit = 2000000.0\n"
            "max_portfolio_concentration = 0.5\n"
            "data_retention_years = 5\n"
            'regulators = ["FINRA"]\n'
        )
    storage = os.path.join(tmp_storage, "storage2")
    cm = load_config_from_toml(config_path, storage)
    assert cm.config.account_id == "acc-toml"
    assert cm.config.base_currency == "USD"
    assert cm.config.large_trade_threshold == 200_000.0
    assert cm.config.regulators == ["FINRA"]
    assert cm.storage_path == storage
    assert cm.trade_count == 0


def test_load_config_from_toml_default_storage(tmp_storage):
    """load_config_from_toml 不传 storage 时,使用 data/compliance/{account_id} 默认路径。"""
    config_path = os.path.join(tmp_storage, "test_config.toml")
    with open(config_path, "w") as f:
        f.write(
            'account_id = "acc-default"\n'
            'base_currency = "USD"\n'
            "large_trade_threshold = 100000.0\n"
            "position_limit = 1000000.0\n"
            "max_portfolio_concentration = 0.4\n"
            "data_retention_years = 7\n"
            'regulators = ["SEC"]\n'
        )
    cm = load_config_from_toml(config_path)
    assert "acc-default" in cm.storage_path


def test_load_config_from_toml_missing_file_raises(tmp_storage):
    """load_config_from_toml 配置文件不存在抛 IOError。"""
    config_path = os.path.join(tmp_storage, "nonexistent.toml")
    with pytest.raises(IOError):
        load_config_from_toml(config_path, tmp_storage)


# ═══════════════════════════════════════════════════════════════════════════
# 异常类型
# ═══════════════════════════════════════════════════════════════════════════


def test_compliance_error_is_exception():
    """ComplianceError 是 Exception 子类,可被 except Exception 捕获。"""
    err = ComplianceError("test message")
    assert isinstance(err, Exception)


def test_compliance_error_args_have_code(compliance):
    """ComplianceError 携带错误码(由 to_py_err 注入)。

    通过 record_trade 触发真实的 InvalidTradeData 错误,
    验证 `args` 第一个元素是错误码字符串。
    """
    with pytest.raises(ComplianceError) as excinfo:
        compliance.record_trade(_sample_trade(quantity=-1.0))
    # to_py_err 用 `new_err((code, msg))` 创建异常;
    # PyO3 把 tuple 元素拆到 args,所以 `args[0] == code`, `args[1] == msg`。
    assert excinfo.value.args[0] == "InvalidTradeData"
    assert "Quantity must be positive" in excinfo.value.args[1]


# ═══════════════════════════════════════════════════════════════════════════
# 持久化/审计完整性
# ═══════════════════════════════════════════════════════════════════


def test_audit_event_count_matches_trades(compliance):
    """每笔 trade 产生 1 个 audit event,计数同步。"""
    for _ in range(5):
        compliance.record_trade(_sample_trade())
    assert compliance.trade_count == 5
    assert compliance.audit_event_count == 5
    assert compliance.verify_audit_integrity() is True
