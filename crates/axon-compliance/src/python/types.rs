//! Python 端合规审计数据类型
//!
//! ## 与 Rust API 的关键差异
//!
//! - **配置 pyclass**:`ComplianceConfig` 在 Rust 端是公开字段 struct,Python 端用
//!   `#[pyclass]` + `#[getter]` 暴露同名属性,Python 端 `__new__` 接受 kwargs
//!   (e.g. `ComplianceConfig(account_id="...", large_trade_threshold=100_000.0)`)。
//! - **枚举一对一**:`TradeSide` / `OrderType` / `LiquidityType` / `TradeStatus` /
//!   `AuditEventType` 全部一对一映射为 pyclass enum,`__str__` 返回小写字符串。
//! - **PyDict 构造 trade**:`PyTradeRecord::from_dict` 接受 Python 端 dict
//!   (side / order_type / status / liquidity 用**字符串**而非枚举),`record_trade`
//!   内部完成 dict → `TradeRecord` 转换(降门槛,Python 端不必 import 5 个枚举)。
//! - **`AuditEvent` 不暴露**:Rust 端 `AuditEvent` 内部字段多且生命周期由
//!   `AuditLog` 链式管理,Python 端用户只关心"审计完整性"和"事件计数",
//!   不必直接构造事件;`ComplianceModule.audit_event_count()` getter 即可。

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::types::{
    AuditEventType as RustAuditEventType, ComplianceConfig as RustConfig,
    LiquidityType as RustLiquidity, OrderType as RustOrderType, TradeSide as RustSide,
    TradeStatus as RustStatus,
};

// ═══════════════════════════════════════════════════════════════════════════
// ComplianceConfig
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端合规配置。
///
/// 用法::
///     cfg = ComplianceConfig(
///         account_id="acc-001",
///         base_currency="USDT",
///         large_trade_threshold=100_000.0,
///         position_limit=1_000_000.0,
///         max_portfolio_concentration=0.4,
///         data_retention_years=7,
///         regulators=["SEC"],
///     )
#[pyclass(name = "ComplianceConfig", skip_from_py_object)]
#[derive(Debug, Clone)]
pub struct PyComplianceConfig(pub RustConfig);

#[pymethods]
impl PyComplianceConfig {
    #[new]
    fn new(
        account_id: String,
        base_currency: String,
        large_trade_threshold: f64,
        position_limit: f64,
        max_portfolio_concentration: f64,
        data_retention_years: u32,
        regulators: Vec<String>,
    ) -> Self {
        Self(RustConfig {
            account_id,
            base_currency,
            large_trade_threshold,
            position_limit,
            max_portfolio_concentration,
            data_retention_years,
            regulators,
        })
    }

    #[getter]
    fn account_id(&self) -> String {
        self.0.account_id.clone()
    }

    #[getter]
    fn base_currency(&self) -> String {
        self.0.base_currency.clone()
    }

    #[getter]
    fn large_trade_threshold(&self) -> f64 {
        self.0.large_trade_threshold
    }

    #[getter]
    fn position_limit(&self) -> f64 {
        self.0.position_limit
    }

    #[getter]
    fn max_portfolio_concentration(&self) -> f64 {
        self.0.max_portfolio_concentration
    }

    #[getter]
    fn data_retention_years(&self) -> u32 {
        self.0.data_retention_years
    }

    #[getter]
    fn regulators(&self) -> Vec<String> {
        self.0.regulators.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "ComplianceConfig(account_id={:?}, base_currency={:?}, regulators={:?})",
            self.0.account_id, self.0.base_currency, self.0.regulators
        )
    }
}

impl From<RustConfig> for PyComplianceConfig {
    fn from(c: RustConfig) -> Self {
        Self(c)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// TradeSide 枚举
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端交易方向枚举(小写字符串 `__str__`)。
#[pyclass(name = "TradeSide", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyTradeSide {
    Buy,
    Sell,
}

impl From<RustSide> for PyTradeSide {
    fn from(s: RustSide) -> Self {
        match s {
            RustSide::Buy => Self::Buy,
            RustSide::Sell => Self::Sell,
        }
    }
}

impl TryFrom<PyTradeSide> for RustSide {
    type Error = PyErr;
    fn try_from(s: PyTradeSide) -> Result<Self, Self::Error> {
        Ok(match s {
            PyTradeSide::Buy => Self::Buy,
            PyTradeSide::Sell => Self::Sell,
        })
    }
}

#[pymethods]
impl PyTradeSide {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }
    fn __repr__(&self) -> String {
        format!("TradeSide.{}", self.__str__())
    }

    /// 字符串 → TradeSide(case-insensitive),失败抛 `ValueError`。
    #[staticmethod]
    fn from_str(s: &str) -> PyResult<Self> {
        match s.to_lowercase().as_str() {
            "buy" => Ok(Self::Buy),
            "sell" => Ok(Self::Sell),
            other => Err(PyValueError::new_err(format!(
                "invalid TradeSide: {other:?} (expected 'buy' / 'sell')"
            ))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// OrderType 枚举
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端订单类型枚举(小写字符串 `__str__`)。
#[pyclass(name = "OrderType", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyOrderType {
    Market,
    Limit,
    StopLoss,
    TakeProfit,
    StopLimit,
    TrailingStop,
}

impl From<RustOrderType> for PyOrderType {
    fn from(t: RustOrderType) -> Self {
        match t {
            RustOrderType::Market => Self::Market,
            RustOrderType::Limit => Self::Limit,
            RustOrderType::StopLoss => Self::StopLoss,
            RustOrderType::TakeProfit => Self::TakeProfit,
            RustOrderType::StopLimit => Self::StopLimit,
            RustOrderType::TrailingStop => Self::TrailingStop,
        }
    }
}

#[pymethods]
impl PyOrderType {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Market => "market",
            Self::Limit => "limit",
            Self::StopLoss => "stop_loss",
            Self::TakeProfit => "take_profit",
            Self::StopLimit => "stop_limit",
            Self::TrailingStop => "trailing_stop",
        }
    }
    fn __repr__(&self) -> String {
        format!("OrderType.{}", self.__str__())
    }

    /// 字符串 → OrderType(case-insensitive),失败抛 `ValueError`。
    #[staticmethod]
    fn from_str(s: &str) -> PyResult<Self> {
        match s.to_lowercase().as_str() {
            "market" => Ok(Self::Market),
            "limit" => Ok(Self::Limit),
            "stop_loss" | "stoploss" => Ok(Self::StopLoss),
            "take_profit" | "takeprofit" => Ok(Self::TakeProfit),
            "stop_limit" | "stoplimit" => Ok(Self::StopLimit),
            "trailing_stop" | "trailingstop" => Ok(Self::TrailingStop),
            other => Err(PyValueError::new_err(format!(
                "invalid OrderType: {other:?}"
            ))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// LiquidityType 枚举
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端流动性类型枚举(小写字符串 `__str__`)。
#[pyclass(name = "LiquidityType", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyLiquidityType {
    Maker,
    Taker,
}

impl From<RustLiquidity> for PyLiquidityType {
    fn from(l: RustLiquidity) -> Self {
        match l {
            RustLiquidity::Maker => Self::Maker,
            RustLiquidity::Taker => Self::Taker,
        }
    }
}

#[pymethods]
impl PyLiquidityType {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Maker => "maker",
            Self::Taker => "taker",
        }
    }
    fn __repr__(&self) -> String {
        format!("LiquidityType.{}", self.__str__())
    }

    #[staticmethod]
    fn from_str(s: &str) -> PyResult<Self> {
        match s.to_lowercase().as_str() {
            "maker" => Ok(Self::Maker),
            "taker" => Ok(Self::Taker),
            other => Err(PyValueError::new_err(format!(
                "invalid LiquidityType: {other:?}"
            ))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// TradeStatus 枚举
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端交易状态枚举。
#[pyclass(name = "TradeStatus", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, PartialEq)]
pub enum PyTradeStatus {
    Pending,
    Filled,
    PartiallyFilled,
    Cancelled,
    Rejected,
}

#[pymethods]
impl PyTradeStatus {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Filled => "filled",
            Self::PartiallyFilled => "partially_filled",
            Self::Cancelled => "cancelled",
            Self::Rejected => "rejected",
        }
    }
    fn __repr__(&self) -> String {
        format!("TradeStatus.{}", self.__str__())
    }

    #[staticmethod]
    fn from_str(s: &str) -> PyResult<Self> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "filled" => Ok(Self::Filled),
            "partially_filled" | "partiallyfilled" => Ok(Self::PartiallyFilled),
            "cancelled" | "canceled" => Ok(Self::Cancelled),
            "rejected" => Ok(Self::Rejected),
            other => Err(PyValueError::new_err(format!(
                "invalid TradeStatus: {other:?}"
            ))),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// AuditEventType 枚举
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端审计事件类型枚举(只读,记录在审计日志里的事件种类)。
#[pyclass(name = "AuditEventType", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyAuditEventType {
    TradeExecuted,
    OrderPlaced,
    OrderCancelled,
    OrderModified,
    PositionOpened,
    PositionClosed,
    StrategyStarted,
    StrategyStopped,
    ConfigChanged,
    UserLogin,
    UserLogout,
    ApiKeyCreated,
    ApiKeyRevoked,
    ReportGenerated,
    DataExported,
    SystemError,
    ComplianceAlert,
}

impl From<RustAuditEventType> for PyAuditEventType {
    fn from(t: RustAuditEventType) -> Self {
        match t {
            RustAuditEventType::TradeExecuted => Self::TradeExecuted,
            RustAuditEventType::OrderPlaced => Self::OrderPlaced,
            RustAuditEventType::OrderCancelled => Self::OrderCancelled,
            RustAuditEventType::OrderModified => Self::OrderModified,
            RustAuditEventType::PositionOpened => Self::PositionOpened,
            RustAuditEventType::PositionClosed => Self::PositionClosed,
            RustAuditEventType::StrategyStarted => Self::StrategyStarted,
            RustAuditEventType::StrategyStopped => Self::StrategyStopped,
            RustAuditEventType::ConfigChanged => Self::ConfigChanged,
            RustAuditEventType::UserLogin => Self::UserLogin,
            RustAuditEventType::UserLogout => Self::UserLogout,
            RustAuditEventType::ApiKeyCreated => Self::ApiKeyCreated,
            RustAuditEventType::ApiKeyRevoked => Self::ApiKeyRevoked,
            RustAuditEventType::ReportGenerated => Self::ReportGenerated,
            RustAuditEventType::DataExported => Self::DataExported,
            RustAuditEventType::SystemError => Self::SystemError,
            RustAuditEventType::ComplianceAlert => Self::ComplianceAlert,
        }
    }
}

#[pymethods]
impl PyAuditEventType {
    fn __str__(&self) -> &'static str {
        match self {
            Self::TradeExecuted => "trade_executed",
            Self::OrderPlaced => "order_placed",
            Self::OrderCancelled => "order_cancelled",
            Self::OrderModified => "order_modified",
            Self::PositionOpened => "position_opened",
            Self::PositionClosed => "position_closed",
            Self::StrategyStarted => "strategy_started",
            Self::StrategyStopped => "strategy_stopped",
            Self::ConfigChanged => "config_changed",
            Self::UserLogin => "user_login",
            Self::UserLogout => "user_logout",
            Self::ApiKeyCreated => "api_key_created",
            Self::ApiKeyRevoked => "api_key_revoked",
            Self::ReportGenerated => "report_generated",
            Self::DataExported => "data_exported",
            Self::SystemError => "system_error",
            Self::ComplianceAlert => "compliance_alert",
        }
    }
    fn __repr__(&self) -> String {
        format!("AuditEventType.{}", self.__str__())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PyTradeRecord(dict → TradeRecord 转换器)
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `TradeRecord` 构造辅助器。
///
/// Rust 端 `TradeRecord` 有 17 个字段(13 必填 + 4 optional),Python 端不必让
/// 用户手工构造所有字段。本类提供:
/// - `from_dict(d)` 从 Python dict 构造(必填字段缺失抛 `KeyError`)
/// - `required_fields()` 类方法返回必填字段名列表
/// - `optional_fields()` 类方法返回 optional 字段名列表
///
/// 设计决策:**不**把 `TradeRecord` 本身暴露为 pyclass(只暴露 dict 协议),
/// 原因:TradeRecord 内有 `DateTime<Utc>` / `Uuid` / `TradeSide` 等需复杂转换
/// 的类型,直接 pyclass 暴露会让 Python 端构造门槛过高。
#[pyclass(name = "TradeRecord", skip_from_py_object)]
pub struct PyTradeRecord;

#[pymethods]
impl PyTradeRecord {
    /// 必填字段名列表(Python 端 `from_dict` 需提供)
    #[staticmethod]
    fn required_fields() -> Vec<&'static str> {
        vec![
            "strategy_id",
            "symbol",
            "side",
            "quantity",
            "price",
            "fee",
            "fee_currency",
            "exchange",
        ]
    }

    /// 可选字段名列表(可缺省)
    #[staticmethod]
    fn optional_fields() -> Vec<&'static str> {
        vec![
            "order_id",
            "trade_id",
            "execution_time",
            "settlement_time",
            "status",
            "order_type",
            "exchange_trade_id",
            "liquidity",
            "realized_pnl",
            "funding_rate",
            "slippage",
        ]
    }

    fn __repr__(&self) -> &'static str {
        "TradeRecord(use record_trade(dict) on ComplianceModule)"
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 内部辅助:Rust Status 转换
// ═══════════════════════════════════════════════════════════════════════════

/// 把字符串 `status` 转 Rust `TradeStatus`(包内 `pub(crate)`)。
pub(crate) fn parse_status(s: Option<&str>) -> PyResult<RustStatus> {
    match s.unwrap_or("filled").to_lowercase().as_str() {
        "pending" => Ok(RustStatus::Pending),
        "filled" => Ok(RustStatus::Filled),
        "partially_filled" | "partiallyfilled" => {
            Ok(RustStatus::PartiallyFilled { filled_qty: 0.0 })
        }
        "cancelled" | "canceled" => Ok(RustStatus::Cancelled),
        "rejected" => Ok(RustStatus::Rejected {
            reason: "unknown".into(),
        }),
        other => Err(PyValueError::new_err(format!("invalid status: {other:?}"))),
    }
}

/// 字符串 → Rust LiquidityType(包内 `pub(crate)`,给 `module.rs` 用)。
pub(crate) fn parse_liquidity(s: Option<&str>) -> PyResult<crate::types::LiquidityType> {
    use crate::types::LiquidityType;
    match s.unwrap_or("taker").to_lowercase().as_str() {
        "maker" => Ok(LiquidityType::Maker),
        "taker" => Ok(LiquidityType::Taker),
        other => Err(PyValueError::new_err(format!(
            "invalid liquidity: {other:?} (expected 'maker' / 'taker')"
        ))),
    }
}

/// 字符串 → Rust OrderType(包内 `pub(crate)`,给 `module.rs` 用)。
pub(crate) fn parse_order_type(s: Option<&str>) -> PyResult<crate::types::OrderType> {
    use crate::types::OrderType;
    match s.unwrap_or("market").to_lowercase().as_str() {
        "market" => Ok(OrderType::Market),
        "limit" => Ok(OrderType::Limit),
        "stop_loss" | "stoploss" => Ok(OrderType::StopLoss),
        "take_profit" | "takeprofit" => Ok(OrderType::TakeProfit),
        "stop_limit" | "stoplimit" => Ok(OrderType::StopLimit),
        "trailing_stop" | "trailingstop" => Ok(OrderType::TrailingStop),
        other => Err(PyValueError::new_err(format!(
            "invalid order_type: {other:?}"
        ))),
    }
}

/// 字符串 → Rust TradeSide(包内 `pub(crate)`,给 `module.rs` 用)。
pub(crate) fn parse_side(s: &str) -> PyResult<crate::types::TradeSide> {
    use crate::types::TradeSide;
    match s.to_lowercase().as_str() {
        "buy" => Ok(TradeSide::Buy),
        "sell" => Ok(TradeSide::Sell),
        other => Err(PyValueError::new_err(format!(
            "invalid side: {other:?} (expected 'buy' / 'sell')"
        ))),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 模块注册
// ═══════════════════════════════════════════════════════════════════════════

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyComplianceConfig>()?;
    parent.add_class::<PyTradeSide>()?;
    parent.add_class::<PyOrderType>()?;
    parent.add_class::<PyLiquidityType>()?;
    parent.add_class::<PyTradeStatus>()?;
    parent.add_class::<PyAuditEventType>()?;
    parent.add_class::<PyTradeRecord>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyDict;

    #[test]
    fn trade_side_roundtrip() {
        let buy = PyTradeSide::Buy;
        let s = buy.__str__();
        assert_eq!(s, "buy");
        let back = PyTradeSide::from_str("BUY").unwrap();
        assert_eq!(back, PyTradeSide::Buy);
        let err = PyTradeSide::from_str("hold");
        assert!(err.is_err());
    }

    #[test]
    fn order_type_str_roundtrip() {
        assert_eq!(
            PyOrderType::from_str("market").unwrap(),
            PyOrderType::Market
        );
        assert_eq!(PyOrderType::from_str("LIMIT").unwrap(), PyOrderType::Limit);
        assert_eq!(
            PyOrderType::from_str("stop_loss").unwrap(),
            PyOrderType::StopLoss
        );
        assert_eq!(
            PyOrderType::from_str("trailingstop").unwrap(),
            PyOrderType::TrailingStop
        );
        assert!(PyOrderType::from_str("foo").is_err());
    }

    #[test]
    fn liquidity_type_str_roundtrip() {
        assert_eq!(
            PyLiquidityType::from_str("maker").unwrap(),
            PyLiquidityType::Maker
        );
        assert_eq!(
            PyLiquidityType::from_str("TAKER").unwrap(),
            PyLiquidityType::Taker
        );
        assert!(PyLiquidityType::from_str("foo").is_err());
    }

    #[test]
    fn trade_record_field_list() {
        let required = PyTradeRecord::required_fields();
        assert!(required.contains(&"symbol"));
        assert!(required.contains(&"side"));
        let optional = PyTradeRecord::optional_fields();
        assert!(optional.contains(&"status"));
        assert!(optional.contains(&"realized_pnl"));
    }

    #[test]
    fn compliance_config_getters() {
        let cfg = PyComplianceConfig(RustConfig {
            account_id: "acc-1".into(),
            base_currency: "USDT".into(),
            large_trade_threshold: 100_000.0,
            position_limit: 1_000_000.0,
            max_portfolio_concentration: 0.4,
            data_retention_years: 7,
            regulators: vec!["SEC".into()],
        });
        assert_eq!(cfg.account_id(), "acc-1");
        assert_eq!(cfg.base_currency(), "USDT");
        assert!((cfg.large_trade_threshold() - 100_000.0).abs() < 1e-9);
        assert_eq!(cfg.position_limit(), 1_000_000.0);
        assert_eq!(cfg.data_retention_years(), 7);
        assert_eq!(cfg.regulators(), vec!["SEC".to_string()]);
    }

    #[test]
    fn audit_event_type_str() {
        let t = PyAuditEventType::TradeExecuted;
        assert_eq!(t.__str__(), "trade_executed");
    }

    #[test]
    fn parse_status_defaults() {
        let s = parse_status(None).unwrap();
        assert!(matches!(s, RustStatus::Filled));
        let s = parse_status(Some("PENDING")).unwrap();
        assert!(matches!(s, RustStatus::Pending));
        assert!(parse_status(Some("foo")).is_err());
    }

    #[test]
    fn dict_field_extraction_via_dict_field_macro() -> PyResult<()> {
        // 验证从 dict 解析 trade 字段(dict_field! 宏可用性)
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("symbol", "BTCUSDT").unwrap();
            d.set_item("quantity", 1.0).unwrap();
            let result: PyResult<(String, f64)> = (|| {
                let sym: String = axon_core::dict_field!(d, "symbol", String);
                let qty: f64 = axon_core::dict_field!(d, "quantity", f64);
                Ok((sym, qty))
            })();
            let (sym, qty) = result.unwrap();
            assert_eq!(sym, "BTCUSDT");
            assert!((qty - 1.0).abs() < 1e-9);
        });
        Ok(())
    }
}
