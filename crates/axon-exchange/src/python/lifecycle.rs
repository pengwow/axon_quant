//! Python 端 `OrderLifecycleManager` —— 订单状态机管理 + 崩溃恢复。
//!
//! 委托 `lifecycle::OrderLifecycleManager`,通过 `dict` 协议注入 order,
//! 用字符串 + dict 表示 `OrderStatus`,避免在 `axon-exchange` 重复
//! `axon-oms::types` 暴露(同 `binance.rs` / `okx.rs` 的 dict 协议)。
//!
//! ## 状态机(简化版)
//!
//! - `"pending"` / `"sent"` / `"acknowledged"`:非终态,留 active
//! - `"partially_filled"` / `"filled"` / `"cancelled"`:终态,移至 history
//! - `"rejected"`:终态,移至 history
//!
//! 终态详情(`filled_qty` / `avg_price` / `reason`)由 status dict 提供:
//! ```python
//! mgr.update_status(order_id, {
//!     "status": "filled",
//!     "filled_qty": "0.1",
//!     "avg_price": "50000",
//! })
//! ```

use std::str::FromStr;

use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use rust_decimal::Decimal;

use crate::lifecycle::OrderLifecycleManager as RustLifecycle;
use crate::types::{
    Order as RustOrder, OrderId as RustOrderId, OrderStatus as RustOrderStatus,
    OrderType as RustOrderType, Side as RustSide, Symbol as RustSymbol, TimeInForce as RustTif,
};

use super::error::to_py_err;

// ═══════════════════════════════════════════════════════════════════════════
// 主类: PyOrderLifecycleManager
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `OrderLifecycleManager` —— 订单状态机。
///
/// 内部持有 `OrderLifecycleManager`(基于 `parking_lot::RwLock<HashMap>`
/// + `parking_lot::RwLock<Vec>`,线程安全)。
///
/// `skip_from_py_object`:Python 端不传 `OrderLifecycleManager` 实例
/// 给其他 Python 函数(只通过构造 + 调方法使用)。
#[pyclass(name = "OrderLifecycleManager", skip_from_py_object)]
pub struct PyOrderLifecycleManager {
    /// Rust 端 `OrderLifecycleManager`
    inner: RustLifecycle,
}

#[pymethods]
impl PyOrderLifecycleManager {
    /// 构造一个空 manager。
    #[new]
    fn new() -> Self {
        Self {
            inner: RustLifecycle::new(),
        }
    }

    /// 注册一个新订单,返回 `client_order_id` (UUID 字符串)。
    ///
    /// dict 必填字段(同 `BinanceAdapter.place_order`):
    /// - `symbol` (str)
    /// - `side` (str): `"buy"` / `"sell"`
    /// - `type` (str): `"market"` / `"limit"`
    /// - `quantity` (str/Decimal)
    /// - `tif` (str): `"GTC"` / `"IOC"` / `"FOK"`
    /// - `exchange` (str): `"binance"` / `"okx"`
    ///
    /// dict 可选字段:
    /// - `price` (str/Decimal,Optional): 限价单必填
    /// - `client_order_id` (str,Optional): 客户端订单 ID(UUID 字符串),
    ///   缺省时自动生成
    ///
    /// **错误**:缺失必填字段 → `PyKeyError`;非法值 → `PyValueError`。
    fn register_order<'py>(&self, order_dict: &Bound<'py, PyDict>) -> PyResult<String> {
        let rust_order = lifecycle_dict_to_order(order_dict)?;
        let id = self.inner.register_order(rust_order);
        Ok(id.to_string())
    }

    /// 更新订单状态。
    ///
    /// Args:
    /// - `order_id` (str): 客户端订单 ID(UUID 字符串)
    /// - `status_dict` (dict): 状态描述,必含 `"status"` 字段
    ///   - `"status"` (str): 状态名
    ///     - 非终态: `"pending"` / `"sent"` / `"acknowledged"`
    ///     - 终态: `"partially_filled"` / `"filled"` / `"cancelled"` / `"rejected"`
    ///   - 终态附加字段:
    ///     - `filled_qty` (str/Decimal): 成交数量(`partially_filled` / `filled` / `cancelled` 必填)
    ///     - `avg_price` (str/Decimal): 成交均价(`partially_filled` / `filled` 必填)
    ///     - `reason` (str): 拒绝原因(`rejected` 必填)
    ///
    /// **错误**:
    /// - `OrderNotFound`:order_id 不在 active 集合
    /// - `PyValueError`:status 字符串无法识别或缺终态字段
    /// - `PyKeyError`:status_dict 缺 `status` 字段
    fn update_status<'py>(&self, order_id: &str, status_dict: &Bound<'py, PyDict>) -> PyResult<()> {
        let oid = parse_order_id(order_id)?;
        let new_status = parse_status(status_dict)?;
        self.inner.update_status(oid, new_status).map_err(to_py_err)
    }

    /// 当前活跃订单数(pending / sent / acknowledged / partially_filled 状态)。
    fn active_count(&self) -> usize {
        self.inner.active_count()
    }

    /// 历史订单数(终态订单:filled / cancelled / rejected)。
    fn history_count(&self) -> usize {
        self.inner.history_count()
    }

    fn __repr__(&self) -> String {
        format!(
            "OrderLifecycleManager(active={}, history={})",
            self.inner.active_count(),
            self.inner.history_count(),
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 解析 helper
// ═══════════════════════════════════════════════════════════════════════════

/// Python dict → Rust [`RustOrder`](生命周期管理器模式)。
///
/// 与 `binance.rs` / `okx.rs` 的 `dict_to_order` 类似,但需要从 dict
/// 中读 `exchange` 字段(因为 lifecycle manager 是跨交易所的,需要
/// caller 显式指定)。Stage 5 内联实现,后续 Stage 6 可考虑提取到
/// 共享 `python/util.rs`(待 `axon-exchange` 依赖关系稳定后再重构)。
fn lifecycle_dict_to_order(dict: &Bound<'_, PyDict>) -> PyResult<RustOrder> {
    let symbol: String = require_field(dict, "symbol")?;
    let side_str: String = require_field(dict, "side")?;
    let side = parse_side(&side_str)?;
    let type_str: String = require_field(dict, "type")?;
    let qty_any: Bound<'_, PyAny> = dict
        .get_item("quantity")?
        .ok_or_else(|| PyKeyError::new_err("missing 'quantity'"))?;
    let quantity = py_to_decimal(&qty_any)?;
    let tif_str: String = require_field(dict, "tif")?;
    let time_in_force = parse_tif(&tif_str)?;
    let exchange_str: String = require_field(dict, "exchange")?;
    let exchange = parse_exchange_id(&exchange_str)?;

    // price: 可选
    let price = if let Some(v) = dict.get_item("price")? {
        Some(py_to_decimal(&v)?)
    } else {
        None
    };

    // client_order_id: 缺省自动生成
    let client_order_id = if let Some(v) = dict.get_item("client_order_id")? {
        let s: String = v.extract()?;
        parse_order_id(&s)?
    } else {
        RustOrderId::new()
    };

    // order_type(支持 market / limit;其他 Stage 5 不暴露)
    let order_type = match type_str.to_lowercase().as_str() {
        "market" => {
            if price.is_some() {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "market order must not have 'price'",
                ));
            }
            RustOrderType::Market
        }
        "limit" => {
            if price.is_none() {
                return Err(PyKeyError::new_err("limit order requires 'price'"));
            }
            RustOrderType::Limit
        }
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "lifecycle unsupported order type: {other} (supported: market / limit)"
            )));
        }
    };

    Ok(RustOrder {
        client_order_id,
        symbol: RustSymbol::new(symbol),
        side,
        order_type,
        price,
        quantity,
        time_in_force,
        exchange,
        meta: std::collections::HashMap::new(),
    })
}

/// Python status dict → Rust [`RustOrderStatus`]
///
/// 支持的 status:
/// - `"pending"` / `"sent"` / `"acknowledged"`: 无附加字段
/// - `"partially_filled"` / `"filled"`: 必填 `filled_qty` + `avg_price`
/// - `"cancelled"`: 必填 `filled_qty`
/// - `"rejected"`: 必填 `reason`
fn parse_status(dict: &Bound<'_, PyDict>) -> PyResult<RustOrderStatus> {
    let status_str: String = require_field(dict, "status")?;
    match status_str.to_lowercase().as_str() {
        "pending" => Ok(RustOrderStatus::Pending),
        "sent" => Ok(RustOrderStatus::Sent),
        "acknowledged" => Ok(RustOrderStatus::Acknowledged),
        "partially_filled" => {
            let filled_qty = require_decimal(dict, "filled_qty")?;
            let avg_price = require_decimal(dict, "avg_price")?;
            Ok(RustOrderStatus::PartiallyFilled {
                filled_qty,
                avg_price,
            })
        }
        "filled" => {
            let filled_qty = require_decimal(dict, "filled_qty")?;
            let avg_price = require_decimal(dict, "avg_price")?;
            Ok(RustOrderStatus::Filled {
                filled_qty,
                avg_price,
            })
        }
        "cancelled" => {
            let filled_qty = require_decimal(dict, "filled_qty")?;
            Ok(RustOrderStatus::Cancelled { filled_qty })
        }
        "rejected" => {
            let reason: String = require_field(dict, "reason")?;
            Ok(RustOrderStatus::Rejected { reason })
        }
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown order status: {other} (supported: pending / sent / acknowledged / \
             partially_filled / filled / cancelled / rejected)"
        ))),
    }
}

/// Python `Decimal` / `int` / `float` / `str` → Rust `Decimal`
fn py_to_decimal(obj: &Bound<'_, PyAny>) -> PyResult<Decimal> {
    let s: String = obj.call_method0("__str__")?.extract()?;
    Decimal::from_str(&s)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid decimal: {e}")))
}

/// `side` 字符串解析
fn parse_side(s: &str) -> PyResult<RustSide> {
    match s.to_lowercase().as_str() {
        "buy" => Ok(RustSide::Buy),
        "sell" => Ok(RustSide::Sell),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "invalid side: {other}"
        ))),
    }
}

/// `tif` 字符串解析
fn parse_tif(s: &str) -> PyResult<RustTif> {
    match s.to_uppercase().as_str() {
        "GTC" => Ok(RustTif::Gtc),
        "IOC" => Ok(RustTif::Ioc),
        "FOK" => Ok(RustTif::Fok),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "invalid tif: {other}"
        ))),
    }
}

/// `exchange` 字符串解析
fn parse_exchange_id(s: &str) -> PyResult<crate::types::ExchangeId> {
    use crate::types::ExchangeId;
    match s.to_lowercase().as_str() {
        "binance" => Ok(ExchangeId::Binance),
        "okx" => Ok(ExchangeId::Okx),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "invalid exchange id: {other} (expected 'binance' or 'okx')"
        ))),
    }
}

/// `order_id` UUID 字符串解析
fn parse_order_id(s: &str) -> PyResult<RustOrderId> {
    uuid::Uuid::from_str(s)
        .map(RustOrderId)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid order id: {e}")))
}

/// 从 dict 取必填字段
fn require_field<'py, T>(dict: &Bound<'py, PyDict>, field: &str) -> PyResult<T>
where
    T: pyo3::conversion::FromPyObjectOwned<'py>,
{
    let v = dict
        .get_item(field)?
        .ok_or_else(|| PyKeyError::new_err(format!("missing '{field}'")))?;
    v.extract::<T>().map_err(|_e| {
        pyo3::exceptions::PyValueError::new_err(format!("field '{field}' has wrong type or value"))
    })
}

/// 从 dict 取必填 Decimal 字段
fn require_decimal(dict: &Bound<'_, PyDict>, field: &str) -> PyResult<Decimal> {
    let v = dict
        .get_item(field)?
        .ok_or_else(|| PyKeyError::new_err(format!("missing '{field}'")))?;
    py_to_decimal(&v)
}

// ═══════════════════════════════════════════════════════════════════════════
// 注册
// ═══════════════════════════════════════════════════════════════════════════

/// 在 `_native.exchange` 下注册 `OrderLifecycleManager`
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyOrderLifecycleManager>()
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// 构造测试用 limit 单 dict
    fn limit_order_dict<'py>(py: Python<'py>, symbol: &str) -> Bound<'py, PyDict> {
        let d = PyDict::new(py);
        d.set_item("symbol", symbol).unwrap();
        d.set_item("side", "buy").unwrap();
        d.set_item("type", "limit").unwrap();
        d.set_item("quantity", "0.1").unwrap();
        d.set_item("price", "50000").unwrap();
        d.set_item("tif", "GTC").unwrap();
        d.set_item("exchange", "binance").unwrap();
        d
    }

    /// 构造 + `__repr__` 显示计数
    #[test]
    fn lifecycle_construct_and_repr() {
        let mgr = PyOrderLifecycleManager::new();
        assert_eq!(mgr.active_count(), 0);
        assert_eq!(mgr.history_count(), 0);
        let r = mgr.__repr__();
        assert!(r.contains("active=0"), "got: {r}");
        assert!(r.contains("history=0"), "got: {r}");
    }

    /// 注册限价单,active_count == 1
    #[test]
    fn lifecycle_register_order_increments_active() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let oid = mgr
                .register_order(&limit_order_dict(py, "BTCUSDT"))
                .unwrap();
            assert!(
                !oid.is_empty(),
                "register_order returned empty client_order_id"
            );
            assert_eq!(mgr.active_count(), 1);
            assert_eq!(mgr.history_count(), 0);
        });
    }

    /// 非终态更新:status 保持 active
    #[test]
    fn lifecycle_update_to_acknowledged_stays_active() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let oid = mgr
                .register_order(&limit_order_dict(py, "BTCUSDT"))
                .unwrap();
            let status = PyDict::new(py);
            status.set_item("status", "acknowledged").unwrap();
            mgr.update_status(&oid, &status).unwrap();
            assert_eq!(mgr.active_count(), 1);
            assert_eq!(mgr.history_count(), 0);
        });
    }

    /// 终态更新:filled 移到 history
    #[test]
    fn lifecycle_update_to_filled_moves_to_history() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let oid = mgr
                .register_order(&limit_order_dict(py, "BTCUSDT"))
                .unwrap();
            let status = PyDict::new(py);
            status.set_item("status", "filled").unwrap();
            status.set_item("filled_qty", "0.1").unwrap();
            status.set_item("avg_price", "50000").unwrap();
            mgr.update_status(&oid, &status).unwrap();
            assert_eq!(mgr.active_count(), 0);
            assert_eq!(mgr.history_count(), 1);
        });
    }

    /// 终态更新:rejected 移到 history
    #[test]
    fn lifecycle_update_to_rejected_moves_to_history() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let oid = mgr
                .register_order(&limit_order_dict(py, "BTCUSDT"))
                .unwrap();
            let status = PyDict::new(py);
            status.set_item("status", "rejected").unwrap();
            status.set_item("reason", "min notional").unwrap();
            mgr.update_status(&oid, &status).unwrap();
            assert_eq!(mgr.active_count(), 0);
            assert_eq!(mgr.history_count(), 1);
        });
    }

    /// 终态更新:cancelled 移到 history
    #[test]
    fn lifecycle_update_to_cancelled_moves_to_history() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let oid = mgr
                .register_order(&limit_order_dict(py, "BTCUSDT"))
                .unwrap();
            let status = PyDict::new(py);
            status.set_item("status", "cancelled").unwrap();
            status.set_item("filled_qty", "0.05").unwrap();
            mgr.update_status(&oid, &status).unwrap();
            assert_eq!(mgr.active_count(), 0);
            assert_eq!(mgr.history_count(), 1);
        });
    }

    /// update_status 找不到 order → OrderNotFound → ExchangeError
    #[test]
    fn lifecycle_update_unknown_order_raises_exchange_error() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let status = PyDict::new(py);
            status.set_item("status", "filled").unwrap();
            status.set_item("filled_qty", "0.1").unwrap();
            status.set_item("avg_price", "50000").unwrap();
            let err = mgr
                .update_status("00000000-0000-0000-0000-000000000000", &status)
                .unwrap_err();
            // ExchangeError 的 args[0] = "OrderNotFound"
            let s = err.value(py).to_string();
            assert!(s.contains("OrderNotFound"), "got: {s}");
        });
    }

    /// status 字符串无法识别 → PyValueError
    #[test]
    fn lifecycle_invalid_status_raises() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let oid = mgr
                .register_order(&limit_order_dict(py, "BTCUSDT"))
                .unwrap();
            let status = PyDict::new(py);
            status.set_item("status", "expired").unwrap();
            let err = mgr.update_status(&oid, &status).unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyValueError>(py));
        });
    }

    /// filled 状态缺 avg_price → PyKeyError
    #[test]
    fn lifecycle_filled_missing_avg_price_raises() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let oid = mgr
                .register_order(&limit_order_dict(py, "BTCUSDT"))
                .unwrap();
            let status = PyDict::new(py);
            status.set_item("status", "filled").unwrap();
            status.set_item("filled_qty", "0.1").unwrap();
            // 缺 avg_price
            let err = mgr.update_status(&oid, &status).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
        });
    }

    /// register 限价单缺 price → PyKeyError
    #[test]
    fn lifecycle_register_limit_missing_price_raises() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("symbol", "BTCUSDT").unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "limit").unwrap();
            d.set_item("quantity", "0.1").unwrap();
            d.set_item("tif", "GTC").unwrap();
            d.set_item("exchange", "binance").unwrap();
            // 缺 price
            let err = mgr.register_order(&d).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
        });
    }

    /// register 缺必填字段 → PyKeyError
    #[test]
    fn lifecycle_register_missing_field_raises() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("symbol", "BTCUSDT").unwrap();
            // 缺 side/type/quantity/tif/exchange
            let err = mgr.register_order(&d).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
        });
    }

    /// `__repr__` 在注册订单后正确更新计数
    #[test]
    fn lifecycle_repr_after_register() {
        let mgr = PyOrderLifecycleManager::new();
        Python::attach(|py| {
            let _oid = mgr
                .register_order(&limit_order_dict(py, "BTCUSDT"))
                .unwrap();
            let r = mgr.__repr__();
            assert!(r.contains("active=1"), "got: {r}");
            assert!(r.contains("history=0"), "got: {r}");
        });
    }

    /// `register` 函数签名稳定
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }

    /// 单元测试辅助:parse_status 直接调(filled)
    #[test]
    fn parse_status_filled_works() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("status", "filled").unwrap();
            d.set_item("filled_qty", "0.1").unwrap();
            d.set_item("avg_price", "50000").unwrap();
            let s = parse_status(&d).unwrap();
            match s {
                RustOrderStatus::Filled {
                    filled_qty,
                    avg_price,
                } => {
                    assert_eq!(filled_qty, dec!(0.1));
                    assert_eq!(avg_price, dec!(50000));
                }
                other => panic!("expected Filled, got {other:?}"),
            }
        });
    }
}
