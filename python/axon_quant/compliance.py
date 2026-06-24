"""axon_quant.compliance 顶层 Python API —— thin wrapper 模式。

把 Rust 端 `axon-compliance` 的 `ComplianceModule` / `ComplianceConfig` /
枚举 / 错误类通过 ``_native.compliance`` 重新导出,保持和其他子模块
(data / backtest / risk / oms / exchange / inference / explain / ensemble)
一致的导入风格。

用法::

    from axon_quant.compliance import (
        ComplianceModule, ComplianceConfig,
        TradeSide, OrderType, LiquidityType, TradeStatus, AuditEventType,
        TradeRecord, ComplianceError, load_config_from_toml,
    )
"""

from __future__ import annotations

from axon_quant._native import compliance as _native_compliance_module  # noqa: E402

# ─── 核心入口类 / 配置 / 工厂 ─────────────────────────────────────────────
ComplianceModule = _native_compliance_module.ComplianceModule
ComplianceConfig = _native_compliance_module.ComplianceConfig
load_config_from_toml = _native_compliance_module.load_config_from_toml

# ─── 枚举类型(小写字符串 __str__)────────────────────────────────────────
TradeSide = _native_compliance_module.TradeSide
OrderType = _native_compliance_module.OrderType
LiquidityType = _native_compliance_module.LiquidityType
TradeStatus = _native_compliance_module.TradeStatus
AuditEventType = _native_compliance_module.AuditEventType

# ─── 辅助: TradeRecord(dict 协议)─────────────────────────────────────────
# Rust 端未把 TradeRecord 暴露为可构造 pyclass,而是采用 dict 协议;
# Python 端 `record_trade({"strategy_id": ..., ...})` 内部解析。
# 这里只导出 `TradeRecord` 引用,提供 required/optional field 元信息。
TradeRecord = _native_compliance_module.TradeRecord

# ─── 异常类型 ───────────────────────────────────────────────────────────
ComplianceError = _native_compliance_module.ComplianceError

__all__ = [
    "ComplianceModule",
    "ComplianceConfig",
    "load_config_from_toml",
    "TradeSide",
    "OrderType",
    "LiquidityType",
    "TradeStatus",
    "AuditEventType",
    "TradeRecord",
    "ComplianceError",
]
