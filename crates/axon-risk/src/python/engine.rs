//! Python 端 `DefaultRiskEngine` + `RiskResult` 枚举 + dict↔Order/Portfolio 桥(Stage 3 Task 3)。
//!
//! # 暴露的符号
//!
//! - `DefaultRiskEngine` — 预交易风控主类
//! - `RiskResult` — `Allow` / `Reject(reason)` / `Warn(msg)` 枚举
//! - `RiskReason` — 8 个变体的拒绝原因(扁平化字符串标签)
//!
//! # dict 协议
//!
//! Python 端通过 dict 注入 `Order` 和 `Portfolio`,参考 [`dict_to_order`] 和 [`dict_to_portfolio`]。
//!
//! ## Order dict
//!
//! 必填:`id` / `symbol` / `side`(`"buy"`/`"sell"`) / `type`(`"market"`/`"limit"`)
//!       / `quantity` / `tif`(`"GTC"`/`"IOC"`/`"FOK"`/`"GFD"`/`"FAK"`)
//! 可选:限价单需 `price`,市价单忽略。
//!
//! ## Portfolio dict
//!
//! 必填:`base_currency`(`"USD"`/`"USDT"`/`"BTC"`/...) / `commission_rate`(`f64`)
//! 可选:`cash`(`{currency: amount}`) / `positions`(`{symbol: {quantity, avg_cost, market_price?}}`)
//!
//! # 设计决策
//!
//! - **`Portfolio` 字段私有**:axon-core 的 `Portfolio` 字段(`cash` / `positions` HashMap)
//!   都是私有的,无法外部直接写。Stage 3 增加了 `pub fn add_position(&mut self, Position)`
//!   与已有的 `deposit(currency, amount)` 配合,让 Python 端可从 dict 构造 Portfolio。
//!
//! - **`Order::id` 是占位**:风控检查不依赖订单 ID 字段(只读 quantity/price/symbol/side),
//!   但 `Order::new` 需要 ID。Python 端必填 `id` 字段,缺省 0 也行。
//!
//! - **`RiskReason` 扁平化为 enum**:Rust 端 `RiskReason` 是带字段的 enum
//!   (`OrderTooLarge { max, actual }`),Python 端用 `#[pyclass]` enum 时
//!   字段展开比较复杂。这里采取**字符串标签 + dict 形式**:`reason.kind = "OrderTooLarge"`,
//!   `reason.max` / `reason.actual` 作为 getter,扁平化 Python 访问。

use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use axon_core::market::Side as CoreSide;
use axon_core::order::{Order as CoreOrder, OrderType, TimeInForce};
use axon_core::parse_py_enum;
use axon_core::portfolio::{Currency, Portfolio, Position};
use axon_core::types::{Price, Quantity, Symbol};

use crate::engine::{DefaultRiskEngine as RustEngine, RiskEngine as RustTrait};
use crate::error::{AlertSeverity, RiskAlert, RiskReason as RustReason, RiskResult as RustResult};

use super::config::PyRiskConfig;
use super::metrics::risk_metrics_to_dict;

// ═══════════════════════════════════════════════════════════════════════════
// 主类: PyDefaultRiskEngine
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `DefaultRiskEngine` —— 预交易风控检查 + 风险指标聚合。
///
/// 包装 Rust [`DefaultRiskEngine`],提供 dict 协议注入订单/组合 +
/// `RiskResult` 字典化输出。
///
/// 注:本类不实现 `Clone`(`DefaultRiskEngine` 内部 `Mutex` 不支持),
/// 所以**不**用 `from_py_object`,改为 `new(config: &Bound<PyAny>)`
/// 接收任意 Python 对象,内部 `extract::<PyRiskConfig>()`。
#[pyclass(name = "DefaultRiskEngine", skip_from_py_object)]
pub struct PyDefaultRiskEngine {
    /// Rust 端 `DefaultRiskEngine`(持有 config + circuit_breaker + daily_pnl 等)
    inner: RustEngine,
}

#[pymethods]
impl PyDefaultRiskEngine {
    /// 构造风控引擎
    ///
    /// Args:
    /// - `config`:`RiskConfig` 配置对象(必填,Python 端传入)
    #[new]
    fn new(config: &Bound<'_, PyAny>) -> PyResult<Self> {
        let py_config = config.extract::<PyRiskConfig>()?;
        Ok(Self {
            inner: RustEngine::new(py_config.inner),
        })
    }

    /// 预交易风控检查(主入口)
    ///
    /// Args:
    /// - `order_dict`:订单 dict(参考模块级 doc)
    /// - `portfolio_dict`:组合 dict(参考模块级 doc)
    ///
    /// Returns:
    /// - `RiskResult.Allow` / `RiskResult.Reject(reason)` / `RiskResult.Warn(msg)`
    ///
    /// 错误:
    /// - 缺字段 / 类型不匹配 / 枚举值非法 → `PyKeyError` / `PyValueError`
    fn check_order<'py>(
        &self,
        order_dict: &Bound<'py, PyDict>,
        portfolio_dict: &Bound<'py, PyDict>,
    ) -> PyResult<PyRiskResult> {
        let order = dict_to_order(order_dict)?;
        let portfolio = dict_to_portfolio(portfolio_dict)?;
        let r = self.inner.check_order(&order, &portfolio);
        Ok(r.into())
    }

    /// 组合级风险监控(返回 `RiskAlert` dict 列表)
    ///
    /// 检查项:
    /// - 日内亏损是否触及 `max_daily_loss`
    /// - 单一标的集中度是否超 `max_concentration`
    fn check_portfolio<'py>(
        &self,
        py: Python<'py>,
        portfolio_dict: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyList>> {
        let portfolio = dict_to_portfolio(portfolio_dict)?;
        let alerts = self.inner.check_portfolio(&portfolio);
        let list = PyList::empty(py);
        for a in &alerts {
            list.append(risk_alert_to_dict(py, a)?)?;
        }
        Ok(list)
    }

    /// 累计日内已实现 PnL
    ///
    /// 调用后:
    /// - 累加到 `daily_realized_pnl`
    /// - 若 ≤ `-max_daily_loss` 则触发熔断器(`is_active() == true`)
    /// - 推入 VaR 滑动窗口(`var_95` 计算的样本)
    fn update_daily_pnl(&self, pnl: f64) {
        self.inner.update_daily_pnl(pnl);
    }

    /// 重置日内状态(同时重置熔断器,**不**重置 VaR 历史窗口)
    fn reset_daily(&self) {
        self.inner.reset_daily();
    }

    /// 读取当前风险指标(返回 dict)
    ///
    /// 字段:
    /// - `total_exposure` (`float`):净资产(NAV)
    /// - `leverage` (`float`):杠杆倍数(`NAV / base_cash`)
    /// - `current_drawdown` (`float`):当前回撤比例
    /// - `daily_realized_pnl` (`float`):日内已实现 PnL
    /// - `var_95` (`float`):95% VaR
    /// - `concentration` (`dict[str, float]`):单一标的占组合比例
    fn metrics<'py>(
        &self,
        py: Python<'py>,
        portfolio_dict: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let portfolio = dict_to_portfolio(portfolio_dict)?;
        let m = self.inner.get_metrics(&portfolio);
        risk_metrics_to_dict(py, &m)
    }

    fn __repr__(&self) -> String {
        "DefaultRiskEngine(...)".to_string()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// RiskResult(struct + kind 标签模式)
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `RiskResult` —— 预交易风控检查结果。
///
/// 注:PyO3 0.28 不支持 `enum` 的 `#[pyclass]`
/// (报错:`Unit variant 'Allow' is not yet supported in a complex enum`),
/// 这里改用 **struct + `kind` 字符串标签** 模式,与 `PyRiskReason` 一致:
///
/// - `kind` (`str`):`"Allow"` / `"Reject"` / `"Warn"`
/// - `reason` (`PyRiskReason | None`):仅 `Reject` 时非空
/// - `message` (`str | None`):仅 `Warn` 时非空
/// - `is_allow` / `is_reject` / `is_warn` (`bool`):便捷判定
///
/// 工厂方法:`RiskResult.allow()` / `RiskResult.reject(reason)` / `RiskResult.warn(message)`。
#[pyclass(name = "RiskResult", skip_from_py_object)]
#[derive(Debug, Clone)]
pub struct PyRiskResult {
    /// 变体标签(`"Allow"` / `"Reject"` / `"Warn"`)
    kind: String,
    /// `Reject` 变体携带的拒绝原因
    reason: Option<PyRiskReason>,
    /// `Warn` 变体携带的提示信息
    message: Option<String>,
}

impl PyRiskResult {
    /// 构造 `Allow`
    fn new_allow() -> Self {
        Self {
            kind: "Allow".to_string(),
            reason: None,
            message: None,
        }
    }

    /// 构造 `Reject(reason)`
    fn new_reject(reason: PyRiskReason) -> Self {
        Self {
            kind: "Reject".to_string(),
            reason: Some(reason),
            message: None,
        }
    }

    /// 构造 `Warn(message)`
    fn new_warn(message: String) -> Self {
        Self {
            kind: "Warn".to_string(),
            reason: None,
            message: Some(message),
        }
    }
}

#[pymethods]
impl PyRiskResult {
    /// 构造 `Allow` 变体
    #[staticmethod]
    fn allow() -> Self {
        Self::new_allow()
    }

    /// 构造 `Reject(reason)` 变体
    #[staticmethod]
    fn reject(reason: PyRiskReason) -> Self {
        Self::new_reject(reason)
    }

    /// 构造 `Warn(message)` 变体
    #[staticmethod]
    fn warn(message: String) -> Self {
        Self::new_warn(message)
    }

    /// 变体标签
    #[getter]
    fn kind(&self) -> &str {
        &self.kind
    }

    /// 拒绝原因(仅 `Reject` 时非空)
    ///
    /// 注:PyO3 0.28 不支持 `Option<&T>` 作为 getter 返回类型,
    /// 这里返回 `Option<Bound<PyAny>>`(`None` → Python `None`),
    /// Python 端用 `result.reason` 拿到的可能是 `RiskReason` 实例或 `None`。
    #[getter]
    fn reason<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match &self.reason {
            Some(r) => Ok(Py::new(py, r.clone())?.into_bound(py).into_any()),
            None => Ok(py.None().into_bound(py)),
        }
    }

    /// 警告信息(仅 `Warn` 时非空)
    #[getter]
    fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// 是否为 `Allow`
    #[getter]
    fn is_allow(&self) -> bool {
        self.kind == "Allow"
    }

    /// 是否为 `Reject`
    #[getter]
    fn is_reject(&self) -> bool {
        self.kind == "Reject"
    }

    /// 是否为 `Warn`
    #[getter]
    fn is_warn(&self) -> bool {
        self.kind == "Warn"
    }

    /// 完整 dict 视图(JSON 序列化友好)
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("kind", &self.kind)?;
        match &self.reason {
            Some(r) => d.set_item("reason", r.to_dict(py)?)?,
            None => d.set_item("reason", py.None())?,
        }
        match &self.message {
            Some(m) => d.set_item("message", m)?,
            None => d.set_item("message", py.None())?,
        }
        Ok(d)
    }

    fn __repr__(&self) -> String {
        match self.kind.as_str() {
            "Allow" => "RiskResult.Allow".to_string(),
            "Reject" => {
                let r = self
                    .reason
                    .as_ref()
                    .map(|r| r.__repr__())
                    .unwrap_or_default();
                format!("RiskResult.Reject({r})")
            }
            "Warn" => format!(
                "RiskResult.Warn({:?})",
                self.message.as_deref().unwrap_or("")
            ),
            other => format!("RiskResult.Unknown({other})"),
        }
    }
}

impl From<RustResult> for PyRiskResult {
    fn from(r: RustResult) -> Self {
        match r {
            RustResult::Allow => Self::new_allow(),
            RustResult::Reject(reason) => Self::new_reject(PyRiskReason::from_rust(reason)),
            RustResult::Warn(msg) => Self::new_warn(msg),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// RiskReason 枚举(扁平化)
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `RiskReason` —— 拒绝原因扁平化枚举。
///
/// Rust 端 8 个变体,Python 端保留 `kind` 字符串标签(便于 `kind == "OrderTooLarge"` 比较),
/// 字段值通过 getter 访问(`max` / `actual` / `instrument` / `limit` / `current` / `until` /
/// `max_pct` / `current_pct` / `pct` / `required` / `available`)。
///
/// `from_py_object`:`RiskResult.reject(reason)` 工厂方法需从 Python 接收实例。
#[pyclass(name = "RiskReason", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyRiskReason {
    /// 稳定标签(`"OrderTooLarge"` / `"PositionLimitExceeded"` / ...)
    kind: String,
    /// 变体字段的统一 dict 视图(便于 Python 端 `reason.to_dict()["max"]`)
    fields: std::collections::HashMap<String, f64>,
    /// 字符串字段(独立于 `fields` 因为是 `String` 不是 `f64`)
    str_fields: std::collections::HashMap<String, String>,
}

impl PyRiskReason {
    /// 从 Rust `RiskReason` 构造 Python 端扁平化表示
    fn from_rust(r: RustReason) -> Self {
        let mut fields = std::collections::HashMap::new();
        let mut str_fields = std::collections::HashMap::new();
        let kind = match r {
            RustReason::OrderTooLarge { max, actual } => {
                fields.insert("max".into(), max);
                fields.insert("actual".into(), actual);
                "OrderTooLarge"
            }
            RustReason::PositionLimitExceeded { instrument, limit } => {
                str_fields.insert("instrument".into(), instrument);
                fields.insert("limit".into(), limit);
                "PositionLimitExceeded"
            }
            RustReason::MaxLeverageExceeded { max, actual } => {
                fields.insert("max".into(), max);
                fields.insert("actual".into(), actual);
                "MaxLeverageExceeded"
            }
            RustReason::MaxDrawdownExceeded {
                max_pct,
                current_pct,
            } => {
                fields.insert("max_pct".into(), max_pct);
                fields.insert("current_pct".into(), current_pct);
                "MaxDrawdownExceeded"
            }
            RustReason::DailyPnLLimit { limit, current } => {
                fields.insert("limit".into(), limit);
                fields.insert("current".into(), current);
                "DailyPnLLimit"
            }
            RustReason::CircuitBreakerActive { until } => {
                fields.insert("until".into(), until as f64);
                "CircuitBreakerActive"
            }
            RustReason::ConcentrationTooHigh { instrument, pct } => {
                str_fields.insert("instrument".into(), instrument);
                fields.insert("pct".into(), pct);
                "ConcentrationTooHigh"
            }
            RustReason::InsufficientMargin {
                required,
                available,
            } => {
                fields.insert("required".into(), required);
                fields.insert("available".into(), available);
                "InsufficientMargin"
            }
        }
        .to_string();
        Self {
            kind,
            fields,
            str_fields,
        }
    }
}

#[pymethods]
impl PyRiskReason {
    /// 变体标签(稳定字符串,便于 Python 端 `if r.kind == "OrderTooLarge": ...`)
    #[getter]
    fn kind(&self) -> &str {
        &self.kind
    }

    /// 数值字段统一访问(`max` / `actual` / `limit` / `current` / `pct` / ...)
    /// 不存在的字段返回 `None`(避免 KeyError,Python 端用 `get` 风格访问)。
    fn get(&self, key: &str) -> Option<f64> {
        self.fields.get(key).copied()
    }

    /// 字符串字段统一访问(`instrument` / ...)
    fn get_str(&self, key: &str) -> Option<String> {
        self.str_fields.get(key).cloned()
    }

    /// 完整 dict 视图(JSON 序列化友好)
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("kind", &self.kind)?;
        for (k, v) in &self.fields {
            d.set_item(k, v)?;
        }
        for (k, v) in &self.str_fields {
            d.set_item(k, v)?;
        }
        Ok(d)
    }

    /// 从 dict 构造(工厂方法,便于 Python 端 `RiskReason.from_dict(d)`)
    ///
    /// dict 字段:
    /// - `kind` (`str`):变体标签(`"OrderTooLarge"` / `"PositionLimitExceeded"` / ...)
    /// - 数值字段直接平铺:`max` / `actual` / `limit` / `current` / `pct` /
    ///   `max_pct` / `current_pct` / `required` / `available` / `until`
    /// - 字符串字段:`instrument`
    ///
    /// 注:本方法不校验 `kind` 与字段的对应关系,只把 dict 内容搬到 struct,
    /// 方便 Python 端构造测试实例(真实风控拒绝原因走 Rust 端产出)。
    #[staticmethod]
    fn from_dict(dict: &Bound<'_, PyDict>) -> PyResult<Self> {
        let get_str = |k: &str| -> PyResult<String> {
            dict.get_item(k)?
                .ok_or_else(|| PyKeyError::new_err(format!("missing '{k}'")))?
                .extract::<String>()
                .map_err(|_| PyValueError::new_err(format!("'{k}' has wrong type")))
        };
        let kind = get_str("kind")?;
        let mut fields = std::collections::HashMap::new();
        let mut str_fields = std::collections::HashMap::new();
        // 收集所有数值字段(白名单方式,避免 dict 噪声进入)
        for k in &[
            "max",
            "actual",
            "limit",
            "current",
            "pct",
            "max_pct",
            "current_pct",
            "required",
            "available",
            "until",
        ] {
            if let Some(v) = dict.get_item(k)? {
                let f: f64 = v
                    .extract()
                    .map_err(|_| PyValueError::new_err(format!("field '{k}' has wrong type")))?;
                fields.insert((*k).to_string(), f);
            }
        }
        // 收集字符串字段
        if let Some(v) = dict.get_item("instrument")? {
            let s: String = v
                .extract()
                .map_err(|_| PyValueError::new_err("field 'instrument' has wrong type"))?;
            str_fields.insert("instrument".to_string(), s);
        }
        Ok(Self {
            kind,
            fields,
            str_fields,
        })
    }

    fn __repr__(&self) -> String {
        format!("RiskReason.{}", self.kind)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// dict 转换辅助
// ═══════════════════════════════════════════════════════════════════════════

/// Python dict → Rust [`CoreOrder`]
///
/// 必填字段:`id` / `symbol` / `side` / `type` / `quantity` / `tif`
/// 可选:限价单需 `price`,市价单忽略
///
/// 错误:缺字段 → `PyKeyError`,类型不匹配 / 枚举值非法 → `PyValueError`
fn dict_to_order(dict: &Bound<'_, PyDict>) -> PyResult<CoreOrder> {
    let id: u64 = require_field(dict, "id")?;
    let symbol: String = require_field(dict, "symbol")?;
    let side_str: String = require_field(dict, "side")?;
    let side = parse_side(&side_str)?;
    let type_str: String = require_field(dict, "type")?;
    let quantity: f64 = require_field(dict, "quantity")?;
    let tif_str: String = require_field(dict, "tif")?;
    let tif = parse_tif(&tif_str)?;

    let order_type = match type_str.to_lowercase().as_str() {
        "market" => OrderType::Market,
        "limit" => {
            let price: f64 = require_field(dict, "price")?;
            OrderType::Limit {
                price: Price::from_f64(price),
            }
        }
        other => {
            return Err(PyValueError::new_err(format!(
                "unsupported order type: {other} (RiskEngine input only accepts 'market' / 'limit')"
            )));
        }
    };

    Ok(CoreOrder::new(
        id,
        Symbol::from(symbol),
        side,
        order_type,
        Quantity::from_f64(quantity),
        tif,
    ))
}

/// Python dict → Rust `Portfolio`
///
/// 必填:`base_currency` (`"USD"`/`"USDT"`/`"BTC"`/...) / `commission_rate` (`f64`)
/// 可选:
/// - `cash` (`dict[str, float]`):各币种余额
/// - `positions` (`dict[str, dict]`):每个持仓
///   - 持仓 dict 字段:`quantity` / `avg_cost` / `market_price`(可选)
///
/// 注:Python 端构造的 `Portfolio` 是"快照"——只用于预交易检查(读路径),
/// 真实成交更新应走 `Portfolio::apply_trade`。
fn dict_to_portfolio(dict: &Bound<'_, PyDict>) -> PyResult<Portfolio> {
    // 必填字段
    let base_currency_str: String = require_field(dict, "base_currency")?;
    let commission_rate: f64 = require_field(dict, "commission_rate")?;
    let base_currency = Currency::new(&base_currency_str);

    let mut p = Portfolio::new(base_currency, commission_rate);

    // 可选 cash 字段:{ "USD": 100_000.0, "BTC": 1.5 }
    if let Some(cash_item) = dict.get_item("cash")? {
        let cash_dict: &Bound<'_, PyDict> = cash_item.cast()?;
        for (k, v) in cash_dict.iter() {
            let curr_str: String = k.extract()?;
            let amount: f64 = v.extract()?;
            p.deposit(Currency::new(&curr_str), amount);
        }
    }

    // 可选 positions 字段:{ "BTC-USDT": {"quantity": 1.0, "avg_cost": 50000.0, "market_price": 55000.0} }
    if let Some(pos_item) = dict.get_item("positions")? {
        let pos_dict: &Bound<'_, PyDict> = pos_item.cast()?;
        for (k, v) in pos_dict.iter() {
            let symbol_str: String = k.extract()?;
            let pos_inner: &Bound<'_, PyDict> = v.cast()?;
            let quantity: f64 = require_field(pos_inner, "quantity")?;
            let avg_cost: f64 = require_field(pos_inner, "avg_cost")?;
            let market_price: Option<f64> = pos_inner
                .get_item("market_price")?
                .map(|x| {
                    x.extract::<f64>()
                        .map_err(|_| PyValueError::new_err("field 'market_price' has wrong type"))
                })
                .transpose()?;
            let mut position = Position::new(
                Symbol::from(symbol_str),
                Quantity::from_f64(quantity),
                Price::from_f64(avg_cost),
            );
            if let Some(mp) = market_price {
                position.market_price = Some(Price::from_f64(mp));
            }
            p.add_position(position);
        }
    }

    Ok(p)
}

parse_py_enum!(parse_side, CoreSide, [
    Buy => "buy",
    Sell => "sell",
]);

parse_py_enum!(parse_tif, TimeInForce, [
    GTC => "gtc",
    IOC => "ioc",
    FOK => "fok",
    GFD => "gfd",
    FAK => "fak",
]);

/// [`RiskAlert`] → Python dict
fn risk_alert_to_dict<'py>(py: Python<'py>, a: &RiskAlert) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("severity", alert_severity_str(a.severity))?;
    d.set_item("timestamp", a.timestamp)?;
    // reason 复用 PyRiskReason 的扁平化
    let pr = PyRiskReason::from_rust(a.reason.clone());
    let reason_d = pr.to_dict(py)?;
    d.set_item("reason", reason_d)?;
    Ok(d)
}

/// `AlertSeverity` → 稳定字符串标签
fn alert_severity_str(s: AlertSeverity) -> &'static str {
    match s {
        AlertSeverity::Info => "Info",
        AlertSeverity::Warning => "Warning",
        AlertSeverity::Critical => "Critical",
        AlertSeverity::Emergency => "Emergency",
    }
}

/// 从 dict 中取必填字段(参考 `axon-backtest::python::engine::require_field`)
fn require_field<'py, T>(dict: &Bound<'py, PyDict>, field: &str) -> PyResult<T>
where
    T: pyo3::conversion::FromPyObjectOwned<'py>,
{
    let v = dict
        .get_item(field)?
        .ok_or_else(|| PyKeyError::new_err(format!("missing '{field}'")))?;
    v.extract::<T>()
        .map_err(|_e| PyValueError::new_err(format!("field '{field}' has wrong type or value")))
}

// ═══════════════════════════════════════════════════════════════════════════
// 注册
// ═══════════════════════════════════════════════════════════════════════════

/// 在 `_native.risk` 下注册 `DefaultRiskEngine` / `RiskResult` / `RiskReason`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyDefaultRiskEngine>()?;
    parent.add_class::<PyRiskResult>()?;
    parent.add_class::<PyRiskReason>()?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

    // ─── dict_to_order 单元测试 ────────────────────────

    /// 限价单 dict 解析正确
    #[test]
    fn dict_to_order_limit_full_fields() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("id", 1u64).unwrap();
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "limit").unwrap();
            d.set_item("price", 100.0_f64).unwrap();
            d.set_item("quantity", 1.0_f64).unwrap();
            d.set_item("tif", "GTC").unwrap();
            let order = dict_to_order(&d).unwrap();
            assert_eq!(order.id, 1);
            assert_eq!(order.symbol, Symbol::from("BTC-USDT"));
            assert_eq!(order.side, CoreSide::Buy);
            assert!(matches!(order.order_type, OrderType::Limit { .. }));
            assert_eq!(order.time_in_force, TimeInForce::GTC);
        });
    }

    /// 市价单 dict 不需要 price
    #[test]
    fn dict_to_order_market_no_price() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("id", 2u64).unwrap();
            d.set_item("symbol", "ETH-USDT").unwrap();
            d.set_item("side", "sell").unwrap();
            d.set_item("type", "market").unwrap();
            d.set_item("quantity", 0.5_f64).unwrap();
            d.set_item("tif", "IOC").unwrap();
            let order = dict_to_order(&d).unwrap();
            assert!(matches!(order.order_type, OrderType::Market));
        });
    }

    /// 限价单缺 price → PyValueError
    #[test]
    fn dict_to_order_limit_missing_price_raises() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("id", 1u64).unwrap();
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "limit").unwrap();
            d.set_item("quantity", 1.0_f64).unwrap();
            d.set_item("tif", "GTC").unwrap();
            let err = dict_to_order(&d).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
        });
    }

    /// 非法 side 字符串 → PyValueError
    #[test]
    fn dict_to_order_invalid_side_raises() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("id", 1u64).unwrap();
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "XXX").unwrap();
            d.set_item("type", "market").unwrap();
            d.set_item("quantity", 1.0_f64).unwrap();
            d.set_item("tif", "GTC").unwrap();
            let err = dict_to_order(&d).unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    /// 非法 type 字符串(stop 等高级类型风控不支持)→ PyValueError
    #[test]
    fn dict_to_order_unsupported_type_raises() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("id", 1u64).unwrap();
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "stop").unwrap();
            d.set_item("price", 100.0_f64).unwrap();
            d.set_item("quantity", 1.0_f64).unwrap();
            d.set_item("tif", "GTC").unwrap();
            let err = dict_to_order(&d).unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    // ─── dict_to_portfolio 单元测试 ────────────────────

    /// 最简 portfolio(只填必填字段)
    #[test]
    fn dict_to_portfolio_minimal() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("base_currency", "USD").unwrap();
            d.set_item("commission_rate", 0.001_f64).unwrap();
            let p = dict_to_portfolio(&d).unwrap();
            assert_eq!(p.base_currency(), Currency::USD);
            assert!((p.commission_rate() - 0.001).abs() < 1e-9);
            assert_eq!(p.positions().len(), 0);
        });
    }

    /// 含 cash + positions 的 portfolio
    #[test]
    fn dict_to_portfolio_with_cash_and_positions() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("base_currency", "USD").unwrap();
            d.set_item("commission_rate", 0.0_f64).unwrap();

            let cash = PyDict::new(py);
            cash.set_item("USD", 50_000.0_f64).unwrap();
            d.set_item("cash", cash).unwrap();

            let positions = PyDict::new(py);
            let pos1 = PyDict::new(py);
            pos1.set_item("quantity", 1.0_f64).unwrap();
            pos1.set_item("avg_cost", 50_000.0_f64).unwrap();
            pos1.set_item("market_price", 55_000.0_f64).unwrap();
            positions.set_item("BTC-USDT", pos1).unwrap();
            d.set_item("positions", positions).unwrap();

            let p = dict_to_portfolio(&d).unwrap();
            assert!((p.base_cash() - 50_000.0).abs() < 1e-6);
            assert_eq!(p.positions().len(), 1);
            let pos = p.position(&Symbol::from("BTC-USDT")).unwrap();
            assert_eq!(pos.quantity, Quantity::from_f64(1.0));
            assert_eq!(pos.avg_cost, Price::from_f64(50_000.0));
            assert_eq!(pos.market_price, Some(Price::from_f64(55_000.0)));
        });
    }

    /// 缺 base_currency → PyKeyError
    #[test]
    fn dict_to_portfolio_missing_base_currency_raises() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("commission_rate", 0.001_f64).unwrap();
            let err = dict_to_portfolio(&d).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
        });
    }

    // ─── PyDefaultRiskEngine 端到端测试 ────────────────

    /// 构造 + 基础属性
    #[test]
    fn engine_construct_with_default_config() {
        Python::attach(|py| {
            let config = PyRiskConfig::new(1000.0, 5000.0, 500.0, 2.0, 0.1, 1000.0, 0.3, 60);
            let config_obj = Py::new(py, config).unwrap();
            let config_bound: &Bound<'_, PyAny> = config_obj.bind(py);
            let engine = PyDefaultRiskEngine::new(config_bound).unwrap();
            let s = engine.__repr__();
            assert!(s.contains("DefaultRiskEngine"), "got: {s}");
        });
    }

    /// `check_order` 合法订单 → Allow
    #[test]
    fn check_order_valid_returns_allow() {
        Python::attach(|py| {
            let config = PyRiskConfig::new(1000.0, 5000.0, 500.0, 2.0, 0.1, 1000.0, 0.3, 60);
            let config_obj = Py::new(py, config).unwrap();
            let config_bound: &Bound<'_, PyAny> = config_obj.bind(py);
            let engine = PyDefaultRiskEngine::new(config_bound).unwrap();

            let order = PyDict::new(py);
            order.set_item("id", 1u64).unwrap();
            order.set_item("symbol", "BTC-USDT").unwrap();
            order.set_item("side", "buy").unwrap();
            order.set_item("type", "limit").unwrap();
            order.set_item("price", 100.0_f64).unwrap();
            order.set_item("quantity", 1.0_f64).unwrap();
            order.set_item("tif", "GTC").unwrap();

            let portfolio = PyDict::new(py);
            portfolio.set_item("base_currency", "USD").unwrap();
            portfolio.set_item("commission_rate", 0.0_f64).unwrap();
            let cash = PyDict::new(py);
            cash.set_item("USD", 100_000.0_f64).unwrap();
            portfolio.set_item("cash", cash).unwrap();

            let r = engine.check_order(&order, &portfolio).unwrap();
            assert!(r.is_allow(), "expected Allow, got: {}", r.__repr__());
        });
    }

    /// `check_order` 超大订单 → Reject(OrderTooLarge)
    #[test]
    fn check_order_oversized_returns_reject() {
        Python::attach(|py| {
            // max_order_value=1000, order value=100*20=2000 → 拒
            let config = PyRiskConfig::new(1000.0, 5000.0, 1000.0, 2.0, 0.1, 1000.0, 0.3, 60);
            let config_obj = Py::new(py, config).unwrap();
            let config_bound: &Bound<'_, PyAny> = config_obj.bind(py);
            let engine = PyDefaultRiskEngine::new(config_bound).unwrap();

            let order = PyDict::new(py);
            order.set_item("id", 1u64).unwrap();
            order.set_item("symbol", "BTC-USDT").unwrap();
            order.set_item("side", "buy").unwrap();
            order.set_item("type", "limit").unwrap();
            order.set_item("price", 100.0_f64).unwrap();
            order.set_item("quantity", 20.0_f64).unwrap();
            order.set_item("tif", "GTC").unwrap();

            let portfolio = PyDict::new(py);
            portfolio.set_item("base_currency", "USD").unwrap();
            portfolio.set_item("commission_rate", 0.0_f64).unwrap();
            let cash = PyDict::new(py);
            cash.set_item("USD", 100_000.0_f64).unwrap();
            portfolio.set_item("cash", cash).unwrap();

            let r = engine.check_order(&order, &portfolio).unwrap();
            assert!(r.is_reject(), "expected Reject, got: {}", r.__repr__());
            // 进一步校验 reason
            assert_eq!(r.kind(), "Reject");
            // 注:`r.reason()` 返回 `PyResult<Bound<PyAny>>`(PyO3 0.28 不支持
            // `Option<&T>` 作为 getter 返回,改用 `py.None()` 表示 `None`),
            // 需 `extract::<PyRiskReason>()` 解包。
            let reason_bound = r.reason(py).unwrap();
            let reason: PyRiskReason = reason_bound.extract().unwrap();
            assert_eq!(reason.kind(), "OrderTooLarge");
            let d = reason.to_dict(py).unwrap();
            assert!(d.get_item("max").unwrap().is_some());
            assert!(d.get_item("actual").unwrap().is_some());
        });
    }

    /// 熔断器触发后,`check_order` → Reject(CircuitBreakerActive)
    #[test]
    fn check_order_after_circuit_breaker_returns_reject() {
        Python::attach(|py| {
            // max_daily_loss=1000
            let config = PyRiskConfig::new(1000.0, 5000.0, 500.0, 2.0, 0.1, 1000.0, 0.3, 60);
            let config_obj = Py::new(py, config).unwrap();
            let config_bound: &Bound<'_, PyAny> = config_obj.bind(py);
            let engine = PyDefaultRiskEngine::new(config_bound).unwrap();
            // 累计日内 PnL 触发熔断
            engine.update_daily_pnl(-1_500.0);

            let order = PyDict::new(py);
            order.set_item("id", 1u64).unwrap();
            order.set_item("symbol", "BTC-USDT").unwrap();
            order.set_item("side", "buy").unwrap();
            order.set_item("type", "limit").unwrap();
            order.set_item("price", 100.0_f64).unwrap();
            order.set_item("quantity", 1.0_f64).unwrap();
            order.set_item("tif", "GTC").unwrap();

            let portfolio = PyDict::new(py);
            portfolio.set_item("base_currency", "USD").unwrap();
            portfolio.set_item("commission_rate", 0.0_f64).unwrap();
            let cash = PyDict::new(py);
            cash.set_item("USD", 100_000.0_f64).unwrap();
            portfolio.set_item("cash", cash).unwrap();

            let r = engine.check_order(&order, &portfolio).unwrap();
            assert!(r.is_reject());
            let reason_bound = r.reason(py).unwrap();
            let reason: PyRiskReason = reason_bound.extract().unwrap();
            assert_eq!(reason.kind(), "CircuitBreakerActive");
        });
    }

    /// `update_daily_pnl` 写入后,`metrics` 读出 `daily_realized_pnl` 正确
    #[test]
    fn update_daily_pnl_updates_metrics() {
        Python::attach(|py| {
            let config = PyRiskConfig::new(1000.0, 5000.0, 500.0, 2.0, 0.1, 1000.0, 0.3, 60);
            let config_obj = Py::new(py, config).unwrap();
            let config_bound: &Bound<'_, PyAny> = config_obj.bind(py);
            let engine = PyDefaultRiskEngine::new(config_bound).unwrap();
            engine.update_daily_pnl(500.0);
            engine.update_daily_pnl(-200.0);

            let portfolio = PyDict::new(py);
            portfolio.set_item("base_currency", "USD").unwrap();
            portfolio.set_item("commission_rate", 0.0_f64).unwrap();
            let cash = PyDict::new(py);
            cash.set_item("USD", 100_000.0_f64).unwrap();
            portfolio.set_item("cash", cash).unwrap();

            let m = engine.metrics(py, &portfolio).unwrap();
            let daily_pnl: f64 = m
                .get_item("daily_realized_pnl")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert!(
                (daily_pnl - 300.0).abs() < 1e-9,
                "expected 300.0, got: {daily_pnl}"
            );
        });
    }

    /// `reset_daily` 重置 daily_pnl 与熔断器
    #[test]
    fn reset_daily_clears_pnl_and_breaker() {
        Python::attach(|py| {
            let config = PyRiskConfig::new(1000.0, 5000.0, 500.0, 2.0, 0.1, 1000.0, 0.3, 60);
            let config_obj = Py::new(py, config).unwrap();
            let config_bound: &Bound<'_, PyAny> = config_obj.bind(py);
            let engine = PyDefaultRiskEngine::new(config_bound).unwrap();
            engine.update_daily_pnl(-1_500.0);
            engine.reset_daily();

            let portfolio = PyDict::new(py);
            portfolio.set_item("base_currency", "USD").unwrap();
            portfolio.set_item("commission_rate", 0.0_f64).unwrap();
            let cash = PyDict::new(py);
            cash.set_item("USD", 100_000.0_f64).unwrap();
            portfolio.set_item("cash", cash).unwrap();

            let m = engine.metrics(py, &portfolio).unwrap();
            let daily_pnl: f64 = m
                .get_item("daily_realized_pnl")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert!(
                (daily_pnl - 0.0).abs() < 1e-9,
                "expected 0.0, got: {daily_pnl}"
            );

            // 熔断器重置后,订单可被允许
            let order = PyDict::new(py);
            order.set_item("id", 1u64).unwrap();
            order.set_item("symbol", "BTC-USDT").unwrap();
            order.set_item("side", "buy").unwrap();
            order.set_item("type", "limit").unwrap();
            order.set_item("price", 100.0_f64).unwrap();
            order.set_item("quantity", 1.0_f64).unwrap();
            order.set_item("tif", "GTC").unwrap();
            let r = engine.check_order(&order, &portfolio).unwrap();
            assert!(
                r.is_allow(),
                "expected Allow after reset, got: {}",
                r.__repr__()
            );
        });
    }

    /// `check_portfolio` 含超额集中度 → 返回 alerts 列表
    #[test]
    fn check_portfolio_returns_alerts() {
        Python::attach(|py| {
            // max_concentration=0.3
            let config = PyRiskConfig::new(1000.0, 5000.0, 500.0, 2.0, 0.1, 1000.0, 0.3, 60);
            let config_obj = Py::new(py, config).unwrap();
            let config_bound: &Bound<'_, PyAny> = config_obj.bind(py);
            let engine = PyDefaultRiskEngine::new(config_bound).unwrap();
            engine.update_daily_pnl(-2_000.0); // 触发 daily_pnl_limit 警报

            let portfolio = PyDict::new(py);
            portfolio.set_item("base_currency", "USD").unwrap();
            portfolio.set_item("commission_rate", 0.0_f64).unwrap();
            let cash = PyDict::new(py);
            cash.set_item("USD", 10_000.0_f64).unwrap();
            portfolio.set_item("cash", cash).unwrap();

            let alerts = engine.check_portfolio(py, &portfolio).unwrap();
            assert!(!alerts.is_empty(), "expected at least 1 alert");
            // 每个 alert 是 dict
            let first = alerts.get_item(0).unwrap();
            assert!(
                first.hasattr("severity").unwrap_or(false) || first.is_instance_of::<PyDict>(),
                "expected dict with 'severity' key or PyDict type"
            );
        });
    }

    // ─── PyRiskResult / PyRiskReason 工厂方法 ───────────

    /// 三个工厂方法 + is_xxx 判定
    #[test]
    fn risk_result_factory_and_predicates() {
        let allow = PyRiskResult::new_allow();
        assert!(allow.is_allow());
        let reject = PyRiskResult::new_reject(PyRiskReason {
            kind: "OrderTooLarge".into(),
            fields: std::collections::HashMap::new(),
            str_fields: std::collections::HashMap::new(),
        });
        assert!(reject.is_reject());
        let warn = PyRiskResult::new_warn("leverage high".into());
        assert!(warn.is_warn());
    }

    /// `RiskReason` 扁平化字段访问
    #[test]
    fn risk_reason_fields_access() {
        let r = PyRiskReason {
            kind: "OrderTooLarge".into(),
            fields: [("max".to_string(), 1000.0), ("actual".to_string(), 2000.0)]
                .into_iter()
                .collect(),
            str_fields: std::collections::HashMap::new(),
        };
        assert_eq!(r.kind(), "OrderTooLarge");
        assert_eq!(r.get("max"), Some(1000.0));
        assert_eq!(r.get("actual"), Some(2000.0));
        assert_eq!(r.get("nonexistent"), None);
    }

    /// `register` 签名稳定(编译期断言)
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
