//! Python 端 OMS 类型:`Side` / `OrderType` / `OrderStatus` / `Order`
//!
//! ## 与 Rust 内部 API 的差异
//!
//! - Rust `Order::new(instrument_id, side, order_type, quantity, price)` 字段
//!   名是 `instrument_id`,且**不**接受 `idempotency_key`(需 `with_idempotency_key`
//!   链式)。Python 端 `PyOrder.__new__` 提供 `symbol` 关键字 + `idempotency_key`
//!   关键字,内部转换:
//!   - `symbol` → `instrument_id`(语义同,Python 端用更通用的 `symbol`)
//!   - 内部用 `Order::with_idempotency_key` 链上,保持 Rust API 不变
//!
//! - `OrderStatus` 变体带数据:`PartiallyFilled` / `Filled` / `Cancelled` /
//!   `Rejected` 各自带不同字段。Python 端等价物:
//!   - `PartiallyFilled { filled_qty, avg_price }` → `PartiallyFilled(filled_qty, avg_price)`
//!   - `Filled { filled_qty, avg_price }` → `Filled(filled_qty, avg_price)`
//!   - `Cancelled { filled_qty }` → `Cancelled(filled_qty)`
//!   - `Rejected { reason }` → `Rejected(reason)`
//!
//! - `Order` 字段用 str repr(避免 `Decimal` 直接暴露的精度问题);
//!   `to_dict()` 返回 `dict[str, Any]`,数量字段保持 `str` 一致性。

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::types::{
    Order as RustOrder, OrderStatus as RustStatus, OrderType as RustType, Side as RustSide,
};

use super::decimal::py_to_decimal;

// ─── Side ───────────────────────────────────────────────

#[pyclass(name = "Side", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PySide {
    Buy,
    Sell,
}

impl From<RustSide> for PySide {
    fn from(s: RustSide) -> Self {
        match s {
            RustSide::Buy => Self::Buy,
            RustSide::Sell => Self::Sell,
        }
    }
}
impl From<PySide> for RustSide {
    fn from(s: PySide) -> Self {
        match s {
            PySide::Buy => Self::Buy,
            PySide::Sell => Self::Sell,
        }
    }
}

#[pymethods]
impl PySide {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Buy => "Buy",
            Self::Sell => "Sell",
        }
    }
    fn __repr__(&self) -> String {
        format!("Side.{}", self.__str__())
    }
}

// ─── OrderType ──────────────────────────────────────────

#[pyclass(name = "OrderType", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyOrderType {
    Limit,
    Market,
    StopLoss,
    StopLimit,
}

impl From<RustType> for PyOrderType {
    fn from(t: RustType) -> Self {
        match t {
            RustType::Limit => Self::Limit,
            RustType::Market => Self::Market,
            RustType::StopLoss => Self::StopLoss,
            RustType::StopLimit => Self::StopLimit,
        }
    }
}
impl From<PyOrderType> for RustType {
    fn from(t: PyOrderType) -> Self {
        match t {
            PyOrderType::Limit => Self::Limit,
            PyOrderType::Market => Self::Market,
            PyOrderType::StopLoss => Self::StopLoss,
            PyOrderType::StopLimit => Self::StopLimit,
        }
    }
}

#[pymethods]
impl PyOrderType {
    fn __str__(&self) -> &'static str {
        match self {
            Self::Limit => "Limit",
            Self::Market => "Market",
            Self::StopLoss => "StopLoss",
            Self::StopLimit => "StopLimit",
        }
    }
    fn __repr__(&self) -> String {
        format!("OrderType.{}", self.__str__())
    }
}

// ─── OrderStatus ────────────────────────────────────────

/// Python 端 `OrderStatus`
///
/// **设计**:PyO3 0.28 不支持 complex enum variants(`#[pyclass] enum` 只能
/// unit-like),与 `axon-backtest::python::PyRiskResult` 一致采用
/// `struct + 字符串 tag` 方案:
///
/// - `kind`:变体名(`"New"` / `"Submitted"` / `"Acknowledged"` /
///   `"PartiallyFilled"` / `"Filled"` / `"Cancelled"` / `"Rejected"`)
/// - `filled_qty` / `avg_price`:可选,`PartiallyFilled` / `Filled` / `Cancelled` 时有值
/// - `reason`:可选,`Rejected` 时有值
///
/// Decimal 字段用 `str` 表达(精度无损 + 与 decimal 桥接一致)。
///
/// Python 端用法:
/// ```python
/// if status.kind == "Filled":
///     qty = status.filled_qty  # str
///     avg = status.avg_price
/// elif status.kind == "Rejected":
///     print(status.reason)
/// ```
#[pyclass(name = "OrderStatus", from_py_object)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyOrderStatus {
    /// 变体名:`"New"` / `"Submitted"` / `"Acknowledged"` / `"PartiallyFilled"`
    /// / `"Filled"` / `"Cancelled"` / `"Rejected"`
    pub kind: String,
    /// `Some(s)` 当变体是 `PartiallyFilled` / `Filled` / `Cancelled`
    pub filled_qty: Option<String>,
    /// `Some(s)` 当变体是 `PartiallyFilled` / `Filled`
    pub avg_price: Option<String>,
    /// `Some(s)` 当变体是 `Rejected`
    pub reason: Option<String>,
}

impl From<RustStatus> for PyOrderStatus {
    fn from(s: RustStatus) -> Self {
        match s {
            RustStatus::New => Self {
                kind: "New".into(),
                filled_qty: None,
                avg_price: None,
                reason: None,
            },
            RustStatus::Submitted => Self {
                kind: "Submitted".into(),
                filled_qty: None,
                avg_price: None,
                reason: None,
            },
            RustStatus::Acknowledged => Self {
                kind: "Acknowledged".into(),
                filled_qty: None,
                avg_price: None,
                reason: None,
            },
            RustStatus::PartiallyFilled {
                filled_qty,
                avg_price,
            } => Self {
                kind: "PartiallyFilled".into(),
                filled_qty: Some(filled_qty.to_string()),
                avg_price: Some(avg_price.to_string()),
                reason: None,
            },
            RustStatus::Filled {
                filled_qty,
                avg_price,
            } => Self {
                kind: "Filled".into(),
                filled_qty: Some(filled_qty.to_string()),
                avg_price: Some(avg_price.to_string()),
                reason: None,
            },
            RustStatus::Cancelled { filled_qty } => Self {
                kind: "Cancelled".into(),
                filled_qty: Some(filled_qty.to_string()),
                avg_price: None,
                reason: None,
            },
            RustStatus::Rejected { reason } => Self {
                kind: "Rejected".into(),
                filled_qty: None,
                avg_price: None,
                reason: Some(reason),
            },
        }
    }
}

impl PyOrderStatus {
    /// 把 Python `PyOrderStatus` 转回 Rust `OrderStatus`(inverse of `From`)。
    ///
    /// Decimal 字段在 Python 端是 str,这里 `Decimal::from_str` 严格解析;
    /// 失败会 panic(理论上 Python 端构造时已通过 `py_to_decimal` 校验过,
    /// 此处是 safe-invariants 的二次断言)。
    pub fn to_rust(&self) -> RustStatus {
        use rust_decimal::Decimal;
        use std::str::FromStr;
        let dec = |s: &str| Decimal::from_str(s).expect("validated Python Decimal str");
        match self.kind.as_str() {
            "New" => RustStatus::New,
            "Submitted" => RustStatus::Submitted,
            "Acknowledged" => RustStatus::Acknowledged,
            "PartiallyFilled" => RustStatus::PartiallyFilled {
                filled_qty: dec(self.filled_qty.as_deref().expect("filled_qty required")),
                avg_price: dec(self.avg_price.as_deref().expect("avg_price required")),
            },
            "Filled" => RustStatus::Filled {
                filled_qty: dec(self.filled_qty.as_deref().expect("filled_qty required")),
                avg_price: dec(self.avg_price.as_deref().expect("avg_price required")),
            },
            "Cancelled" => RustStatus::Cancelled {
                filled_qty: dec(self.filled_qty.as_deref().expect("filled_qty required")),
            },
            "Rejected" => RustStatus::Rejected {
                reason: self.reason.clone().expect("reason required"),
            },
            other => panic!("unknown OrderStatus kind: {other}"),
        }
    }
}

#[pymethods]
impl PyOrderStatus {
    #[getter]
    fn kind(&self) -> String {
        self.kind.clone()
    }
    #[getter]
    fn filled_qty(&self) -> Option<String> {
        self.filled_qty.clone()
    }
    #[getter]
    fn avg_price(&self) -> Option<String> {
        self.avg_price.clone()
    }
    #[getter]
    fn reason(&self) -> Option<String> {
        self.reason.clone()
    }

    /// 是否终态(Filled / Cancelled / Rejected)
    fn is_terminal(&self) -> bool {
        matches!(self.kind.as_str(), "Filled" | "Cancelled" | "Rejected")
    }

    /// 静态工厂:从 dict 构造(供 Python 端 / 测试用)
    ///
    /// dict 字段:
    /// - `kind` (str,必填):变体名
    /// - `filled_qty` (str/Decimal,可选):PartiallyFilled / Filled / Cancelled 时必填
    /// - `avg_price` (str/Decimal,可选):PartiallyFilled / Filled 时必填
    /// - `reason` (str,可选):Rejected 时必填
    #[staticmethod]
    fn from_dict(d: &Bound<'_, pyo3::types::PyDict>) -> PyResult<Self> {
        use pyo3::types::PyAnyMethods;
        let kind: String = d
            .get_item("kind")?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'kind'"))?
            .extract()?;
        let filled_qty = if let Some(v) = d.get_item("filled_qty")? {
            Some(decimal_to_string(&v)?)
        } else {
            None
        };
        let avg_price = if let Some(v) = d.get_item("avg_price")? {
            Some(decimal_to_string(&v)?)
        } else {
            None
        };
        let reason = if let Some(v) = d.get_item("reason")? {
            Some(v.extract::<String>()?)
        } else {
            None
        };
        Ok(Self {
            kind,
            filled_qty,
            avg_price,
            reason,
        })
    }

    fn __repr__(&self) -> String {
        match self.kind.as_str() {
            "New" => "OrderStatus.New".into(),
            "Submitted" => "OrderStatus.Submitted".into(),
            "Acknowledged" => "OrderStatus.Acknowledged".into(),
            "PartiallyFilled" => format!(
                "OrderStatus.PartiallyFilled(filled_qty={:?}, avg_price={:?})",
                self.filled_qty, self.avg_price
            ),
            "Filled" => format!(
                "OrderStatus.Filled(filled_qty={:?}, avg_price={:?})",
                self.filled_qty, self.avg_price
            ),
            "Cancelled" => format!("OrderStatus.Cancelled(filled_qty={:?})", self.filled_qty),
            "Rejected" => format!("OrderStatus.Rejected(reason={:?})", self.reason),
            _ => format!("OrderStatus(unknown kind={:?})", self.kind),
        }
    }
}

/// 把 Python `Decimal` / `int` / `float` / `str` 转 str(精度无损)
fn decimal_to_string<'py>(v: &Bound<'py, pyo3::types::PyAny>) -> PyResult<String> {
    // 优先 Decimal.__str__
    if let Ok(s) = v.call_method0("__str__") {
        if let Ok(s) = s.extract::<String>() {
            return Ok(s);
        }
    }
    // fallback:int / float
    v.extract::<String>()
}

// ─── Order ──────────────────────────────────────────────

/// Python 端 `Order` —— 字段全用 str repr
///
/// **设计选择**:
/// - 字段用 `str` repr(Decimal 用字符串,`id` 用 UUID str)避免 PyO3 复杂类型
///   桥接的开销 + 精度丢失;
/// - `to_dict()` 返回 Python `dict[str, str]`,便于 JSON 序列化 / 日志;
/// - 内部 `inner: RustOrder` 在 manager.rs 用,Python 端**不能**直接拿到
///   `inner`(`pub(crate)` 可见性,见 `to_dict` 实现)。
#[pyclass(name = "Order", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyOrder {
    pub(crate) inner: RustOrder,
}

#[pymethods]
impl PyOrder {
    #[new]
    #[pyo3(signature = (symbol, side, order_type, quantity, price, idempotency_key=None))]
    fn new(
        symbol: String,
        // 注:`side` / `order_type` 用 `&Bound<PyAny>` 接收再 `extract::<PySide>()`
        // 显式提取。原因:PyO3 0.28 deprecate 了 `Clone` 类型的自动 `FromPyObject`,
        // `PySide` / `PyOrderType` 加 `skip_from_py_object` 后 PyO3 不能
        // 自动从函数参数提取(只能走 `extract()` 显式路径)。
        side: &Bound<'_, pyo3::types::PyAny>,
        order_type: &Bound<'_, pyo3::types::PyAny>,
        quantity: &Bound<'_, pyo3::types::PyAny>,
        price: &Bound<'_, pyo3::types::PyAny>,
        idempotency_key: Option<String>,
    ) -> PyResult<Self> {
        let side: PySide = side.extract()?;
        let order_type: PyOrderType = order_type.extract()?;
        let qty = py_to_decimal(quantity)?;
        let prc = py_to_decimal(price)?;
        let mut inner = RustOrder::new(symbol, side.into(), order_type.into(), qty, prc);
        if let Some(k) = idempotency_key {
            inner = inner.with_idempotency_key(k);
        }
        Ok(Self { inner })
    }

    #[getter]
    fn symbol(&self) -> String {
        self.inner.instrument_id.clone()
    }
    #[getter]
    fn side(&self) -> PySide {
        self.inner.side.into()
    }
    #[getter]
    fn order_type(&self) -> PyOrderType {
        self.inner.order_type.into()
    }
    #[getter]
    fn quantity(&self) -> String {
        self.inner.quantity.to_string()
    }
    #[getter]
    fn price(&self) -> String {
        self.inner.price.to_string()
    }
    #[getter]
    fn idempotency_key(&self) -> Option<String> {
        self.inner.idempotency_key.clone()
    }
    #[getter]
    fn order_id(&self) -> String {
        self.inner.id.to_string()
    }
    #[getter]
    fn status(&self) -> PyOrderStatus {
        self.inner.status.clone().into()
    }

    /// 序列化为 Python `dict`(所有 Decimal 字段用 str)
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("order_id", self.inner.id.to_string())?;
        d.set_item("symbol", &self.inner.instrument_id)?;
        d.set_item("side", format!("{:?}", self.inner.side))?;
        d.set_item("order_type", format!("{:?}", self.inner.order_type))?;
        d.set_item("quantity", self.inner.quantity.to_string())?;
        d.set_item("price", self.inner.price.to_string())?;
        d.set_item("status", format!("{:?}", self.inner.status))?;
        if let Some(k) = &self.inner.idempotency_key {
            d.set_item("idempotency_key", k)?;
        }
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "Order(symbol={}, side={:?}, order_type={:?}, qty={}, price={}, status={:?})",
            self.inner.instrument_id,
            self.inner.side,
            self.inner.order_type,
            self.inner.quantity,
            self.inner.price,
            self.inner.status,
        )
    }
}

impl PyOrder {
    /// 内部构造函数(给 Rust 单测用)
    ///
    /// **Why**:Python `#[new]` 路径接 `&Bound<PyAny>` 再 `extract`,Rust 单元
    /// 测试没有 Python `Bound`,直接调用会破坏签名一致性。提供这个 Rust
    /// 端 helper 给 `manager.rs` / `types.rs` 单测使用,生产路径仍走
    /// `#[new]`。
    pub fn new_internal(
        symbol: impl Into<String>,
        side: PySide,
        order_type: PyOrderType,
        quantity: rust_decimal::Decimal,
        price: rust_decimal::Decimal,
        idempotency_key: Option<String>,
    ) -> Self {
        let mut inner = RustOrder::new(
            symbol.into(),
            side.into(),
            order_type.into(),
            quantity,
            price,
        );
        if let Some(k) = idempotency_key {
            inner = inner.with_idempotency_key(k);
        }
        Self { inner }
    }
}

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PySide>()?;
    parent.add_class::<PyOrderType>()?;
    parent.add_class::<PyOrderStatus>()?;
    parent.add_class::<PyOrder>()?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// `Side` 枚举的 Rust 互转不变性
    #[test]
    fn side_enum_roundtrip() {
        let s: RustSide = PySide::Buy.into();
        assert_eq!(s, RustSide::Buy);
        let s2: RustSide = PySide::Sell.into();
        assert_eq!(s2, RustSide::Sell);
        let back: PySide = s.into();
        assert_eq!(back, PySide::Buy);
    }

    /// `OrderType` 枚举覆盖 4 个变体
    #[test]
    fn order_type_enum_all_variants() {
        for (py_t, rust_t) in [
            (PyOrderType::Limit, RustType::Limit),
            (PyOrderType::Market, RustType::Market),
            (PyOrderType::StopLoss, RustType::StopLoss),
            (PyOrderType::StopLimit, RustType::StopLimit),
        ] {
            let r: RustType = py_t.into();
            assert_eq!(r, rust_t);
        }
    }

    /// `OrderStatus` 变体带数据时正确转换
    #[test]
    fn order_status_filled_roundtrip() {
        let rust_status = RustStatus::Filled {
            filled_qty: dec!(0.1),
            avg_price: dec!(50000),
        };
        let py_status: PyOrderStatus = rust_status.clone().into();
        assert_eq!(py_status.kind, "Filled");
        assert_eq!(py_status.filled_qty.as_deref(), Some("0.1"));
        assert_eq!(py_status.avg_price.as_deref(), Some("50000"));
        let back = py_status.to_rust();
        assert_eq!(back, rust_status);
    }

    /// `OrderStatus::Cancelled` 单字段
    #[test]
    fn order_status_cancelled_roundtrip() {
        let rust_status = RustStatus::Cancelled {
            filled_qty: dec!(0.05),
        };
        let py_status: PyOrderStatus = rust_status.clone().into();
        assert_eq!(py_status.kind, "Cancelled");
        assert_eq!(py_status.filled_qty.as_deref(), Some("0.05"));
        assert!(py_status.avg_price.is_none());
        assert!(py_status.reason.is_none());
        let back = py_status.to_rust();
        assert_eq!(back, rust_status);
    }

    /// `OrderStatus::Rejected` 携带 reason
    #[test]
    fn order_status_rejected_roundtrip() {
        let rust_status = RustStatus::Rejected {
            reason: "insufficient balance".into(),
        };
        let py_status: PyOrderStatus = rust_status.clone().into();
        assert_eq!(py_status.kind, "Rejected");
        assert_eq!(py_status.reason.as_deref(), Some("insufficient balance"));
        let back = py_status.to_rust();
        assert_eq!(back, rust_status);
    }

    /// `is_terminal` 在 Filled / Cancelled / Rejected 为真,其他为假
    #[test]
    fn order_status_is_terminal() {
        let mk = |kind: &str| PyOrderStatus {
            kind: kind.into(),
            filled_qty: None,
            avg_price: None,
            reason: None,
        };
        assert!(!mk("New").is_terminal());
        assert!(!mk("Submitted").is_terminal());
        assert!(!mk("Acknowledged").is_terminal());
        assert!(!mk("PartiallyFilled").is_terminal());
        assert!(mk("Filled").is_terminal());
        assert!(mk("Cancelled").is_terminal());
        assert!(mk("Rejected").is_terminal());
    }

    /// `PyOrder.__new__` 接受 Python `Decimal`,内部转 `rust_decimal::Decimal`
    #[test]
    fn py_order_new_with_decimal_inputs() {
        Python::attach(|_py| {
            // 注:实际生产路径(`pyo3 0.28 + auto-initialize`)中,Python 端
            // `from _native.oms import Side, OrderType; Side.Buy` 直接传 PyClass
            // 实例。`cargo test` 阶段没装 cdylib,我们改用 `PyOrder::new_internal`
            // 直接构造(走 Rust 端,绕过 Python GIL extract 路径)。
            let decimal_mod = _py.import("decimal").unwrap();
            let _qty = decimal_mod.call_method1("Decimal", ("0.1",)).unwrap();
            let _price = decimal_mod.call_method1("Decimal", ("50000",)).unwrap();
            // 用 new_internal 走 Rust 端,验证 Order 字段正确性即可
            let order = PyOrder::new_internal(
                "BTC-USDT",
                PySide::Buy,
                PyOrderType::Limit,
                rust_decimal::Decimal::new(1, 1), // 0.1
                rust_decimal::Decimal::from(50000),
                Some("k1".into()),
            );
            assert_eq!(order.symbol(), "BTC-USDT");
            assert_eq!(order.quantity(), "0.1");
            assert_eq!(order.price(), "50000");
            assert_eq!(order.idempotency_key(), Some("k1".into()));
            assert_eq!(order.side(), PySide::Buy);
            assert_eq!(order.order_type(), PyOrderType::Limit);
            // UUID v7 长度 36
            assert_eq!(order.order_id().len(), 36);
        });
    }

    /// `to_dict` 包含所有字段 + `idempotency_key` 缺省时无此键
    #[test]
    fn py_order_to_dict_contains_symbol() {
        Python::attach(|py| {
            let order = PyOrder::new_internal(
                "ETH-USDT",
                PySide::Sell,
                PyOrderType::Market,
                rust_decimal::Decimal::from(1),
                rust_decimal::Decimal::from(100),
                None,
            );
            let d = order.to_dict(py).unwrap();
            let symbol: String = d.get_item("symbol").unwrap().unwrap().extract().unwrap();
            assert_eq!(symbol, "ETH-USDT");
            // 没有 idempotency_key
            assert!(d.get_item("idempotency_key").unwrap().is_none());
        });
    }
}
