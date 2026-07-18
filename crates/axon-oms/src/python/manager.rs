//! Python 端 `OrderManager` —— 订单生命周期管理 + 批量操作 + 快照。
//!
//! ## 与 Rust API 的关键差异
//!
//! - Rust 端 `OrderManager` 内部用 `parking_lot::RwLock` 保护所有字段,
//!   **所有方法接收 `&self`**(而非 `&mut self`)。Python 端 `PyOrderManager`
//!   的所有方法也接收 `&self`,PyO3 会自动多线程安全。
//!
//! - `snapshot()` Rust 返回 `OmsSnapshot`(含 `active_orders` HashMap +
//!   `order_history` Vec + `version` + `timestamp` + `portfolio` Option),
//!   结构复杂。Python 端简化为 `dict`:
//!   `{"version": int, "active_orders": dict[str, str], "history_count": int}`
//!   —— `active_orders` 把每个 order 序列化为 `repr(order.status)` 字符串,
//!   便于 Python 端判断状态机位置,详细数据走 `get_order_status` 查。
//!
//! - `batch_submit` Rust 没有,Rust 仅支持单个 `submit`(因内部 lock pattern
//!   不允许 batch atomicity)。Python 端 `batch_submit` 是**语义糖**,循环
//!   `submit` 收集结果。**注意**:若有任一 submit 失败,部分订单已提交,
//!   Python 端收集到的 id 列表会少 —— 这是与"原子批量"语义的差异,文档
//!   中显式标注。
//!
//! - `update_status` 接受 `PyOrderStatus`(Python 端 struct + 字符串 tag),
//!   内部用 `to_rust()` 转回 `RustStatus`。
//!
//! - 订单 ID 是 UUID,Rust `OrderId(Uuid)` 实现 `Display` 输出标准 UUID
//!   字符串 (36 字符)。Python 端用 `str` 表示。

use std::str::FromStr;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::manager::OrderManager as RustManager;
use crate::types::OrderId as RustOrderId;

use super::error::to_py_err;
use super::portfolio::{portfolio_to_dict, wrap_position};
use super::types::{PyOrder, PyOrderStatus};

/// Python 端 `OrderManager`
///
/// **多线程安全**:内部用 `parking_lot::RwLock`,所有方法 `&self` 即可。
#[pyclass(name = "OrderManager")]
pub struct PyOrderManager {
    inner: RustManager,
}

#[pymethods]
impl PyOrderManager {
    #[new]
    fn new() -> Self {
        Self {
            inner: RustManager::new(),
        }
    }

    /// 初始存款(OMS 启动时调,余额按 currency 累加)
    fn deposit(&self, currency: &str, amount: &Bound<'_, pyo3::types::PyAny>) -> PyResult<()> {
        let amt = super::portfolio::parse_decimal_helper(amount)?;
        self.inner.deposit(currency, amt);
        Ok(())
    }

    /// 取出现金(出金),余额不足时抛 ValueError
    fn withdraw(&self, currency: &str, amount: &Bound<'_, pyo3::types::PyAny>) -> PyResult<()> {
        let amt = super::portfolio::parse_decimal_helper(amount)?;
        self.inner
            .withdraw(currency, amt)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        Ok(())
    }

    /// 提交订单,返回 `order_id` (str,UUID 36 字符)
    fn submit(&self, order: PyOrder) -> PyResult<String> {
        let id = self.inner.submit(order.inner).map_err(to_py_err)?;
        Ok(id.to_string())
    }

    /// 取消订单
    fn cancel(&self, order_id: &str) -> PyResult<()> {
        let id = parse_order_id(order_id)?;
        self.inner.cancel(id).map_err(to_py_err)
    }

    /// 更新订单状态
    fn update_status(&self, order_id: &str, status: PyOrderStatus) -> PyResult<()> {
        let id = parse_order_id(order_id)?;
        let rust_status = status.to_rust();
        self.inner.update_status(id, rust_status).map_err(to_py_err)
    }

    /// 查询订单当前状态(`None` 表示订单不在 active 集合,可能已完成 / 已取消 / 不存在)
    fn get_order_status(&self, order_id: &str) -> PyResult<Option<PyOrderStatus>> {
        let id = parse_order_id(order_id)?;
        Ok(self.inner.get_order_status(id).map(Into::into))
    }

    /// 批量下单(语义糖,循环 submit)
    ///
    /// **注意**:Rust 端没有 atomic batch 语义,若中间某个 submit 失败,前面的
    /// 订单已提交,返回的 id 列表会短于输入。Python 端捕获 OmsError 后,已
    /// 成功的部分仍需 `cancel` 显式回滚(若需)。
    fn batch_submit(&self, orders: Vec<PyOrder>) -> PyResult<Vec<String>> {
        let mut ids = Vec::with_capacity(orders.len());
        for order in orders {
            let id = self.inner.submit(order.inner).map_err(to_py_err)?;
            ids.push(id.to_string());
        }
        Ok(ids)
    }

    /// 当前 active 订单数
    fn active_count(&self) -> usize {
        self.inner.active_count()
    }

    /// 历史订单数
    fn history_count(&self) -> usize {
        self.inner.history_count()
    }

    /// OMS 完整快照(简化版)
    ///
    /// 返回 `dict`:
    /// - `version`:快照版本号(u64,自增)
    /// - `active_orders`:`dict[str, str]`,`order_id → status repr` 字符串
    /// - `history_count`:历史订单数
    ///
    /// **简化策略**:不暴露 `order_history` Vec(可能很大,GB 级),
    /// 不暴露 `timestamp`(与 OmsError 的 portfolio 字段对 Python 端无用);
    /// 详细 portfolio 数据走 `snapshot_balance()` / `snapshot_positions()`
    /// 单独查询(在 Task 5 portfolio.rs 实现)。
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let snap = self.inner.snapshot();
        let d = PyDict::new(py);
        d.set_item("version", snap.version)?;

        // active_orders: order_id (str) -> status repr (str)
        let active = PyDict::new(py);
        for (id, order) in &snap.active_orders {
            active.set_item(id.to_string(), format!("{:?}", order.status))?;
        }
        d.set_item("active_orders", active)?;

        d.set_item("history_count", snap.order_history.len())?;
        Ok(d)
    }

    /// 余额 + 持仓快照(portfolio 段)
    ///
    /// 借 Rust `OrderManager::snapshot_balance` 桥接。返回 `dict`:
    /// - `cash`:`dict[str, str]`,币种 → USDT 数量(Decimal str)
    /// - `positions`:`dict[str, Position]`,symbol → 持仓详情
    /// - `as_of`:快照时间(RFC 3339 字符串)
    ///
    /// **注意**:这是"读时"快照,与 `snapshot()` 共享内部 `RwLock`,但
    /// `version` 字段不会自增(只读路径)。Python 端需要版本号请用
    /// `snapshot()`。
    fn snapshot_balance<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let snap = self.inner.snapshot_balance();
        portfolio_to_dict(py, &snap)
    }

    /// 持仓列表(简化版,无 cash)
    ///
    /// 返回 `list[Position]`,与 `snapshot_balance()["positions"]` 内容
    /// 一致,只是封装为 list 便于 `len()` / `for pos in positions`。
    ///
    /// **性能**:每次调用 clone 整个 positions HashMap;高频调用请缓存。
    fn snapshot_positions<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let positions = self.inner.snapshot_positions();
        let list = PyList::empty(py);
        for pos in positions {
            list.append(wrap_position(pos))?;
        }
        Ok(list)
    }

    /// 处理一个 fill 事件:状态机转移 + portfolio 更新
    ///
    /// **签名**:接受 fill 字段(与 `PyPortfolio.apply_fill` 一致),
    /// 内部构造 `Fill` 结构。`quantity > 0` 表示 buy,`< 0` 表示 sell。
    /// `timestamp=None` 时用 `Utc::now()`。
    ///
    /// **错误**:返回 `OmsError` —— `OrderNotFound` / `InvalidTransition` /
    /// `Portfolio`(底层 portfolio 错误,比如 `InsufficientCash`)。
    #[pyo3(signature = (order_id, fill_id, symbol, price, quantity, fee, timestamp=None))]
    #[allow(clippy::too_many_arguments)]
    fn add_fill(
        &self,
        order_id: &str,
        fill_id: String,
        symbol: String,
        price: &Bound<'_, pyo3::types::PyAny>,
        quantity: &Bound<'_, pyo3::types::PyAny>,
        fee: &Bound<'_, pyo3::types::PyAny>,
        timestamp: Option<String>,
    ) -> PyResult<()> {
        let id = parse_order_id(order_id)?;
        let price_dec = super::portfolio::parse_decimal_helper(price)?;
        let qty_dec = super::portfolio::parse_decimal_helper(quantity)?;
        let fee_dec = super::portfolio::parse_decimal_helper(fee)?;
        let ts = match timestamp {
            Some(s) => super::portfolio::parse_ts_helper(&s)?,
            None => chrono::Utc::now(),
        };
        let fill = crate::types::Fill {
            fill_id,
            symbol,
            instrument: None,
            price: price_dec,
            quantity: qty_dec,
            fee: fee_dec,
            timestamp: ts,
        };
        self.inner.add_fill(id, fill).map_err(to_py_err)
    }

    fn __repr__(&self) -> String {
        format!(
            "OrderManager(active={}, history={})",
            self.inner.active_count(),
            self.inner.history_count(),
        )
    }
}

/// 解析 Python 端传入的 `order_id` (str) → Rust `OrderId`
///
/// 失败 → `PyValueError`(预期:Python 端传了非 UUID 格式的字符串)。
fn parse_order_id(s: &str) -> PyResult<RustOrderId> {
    UuidWrapper::parse(s)
        .map(RustOrderId)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid order id: {e}")))
}

/// 简单 wrapper 来转 `uuid::Error` 成 `String`(避免在测试模块外引用 uuid)
struct UuidWrapper;

impl UuidWrapper {
    fn parse(s: &str) -> Result<uuid::Uuid, String> {
        uuid::Uuid::from_str(s).map_err(|e| e.to_string())
    }
}

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyOrderManager>()
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// 构造测试用 `PyOrder`(BTC-USDT Buy Limit 0.1 @ 50000)
    fn build_order(_py: Python<'_>, key: Option<&str>) -> PyOrder {
        PyOrder::new_internal(
            "BTC-USDT",
            super::super::types::PySide::Buy,
            super::super::types::PyOrderType::Limit,
            dec!(0.1),
            dec!(50000),
            key.map(String::from),
        )
    }

    /// `OrderManager::new()` 创建空 manager
    #[test]
    fn order_manager_new_is_empty() {
        let mgr = PyOrderManager::new();
        assert_eq!(mgr.active_count(), 0);
        assert_eq!(mgr.history_count(), 0);
    }

    /// `submit` 返回 UUID 36 字符字符串
    #[test]
    fn order_manager_submit_returns_uuid_str() {
        Python::attach(|py| {
            let mgr = PyOrderManager::new();
            let order = build_order(py, None);
            let id = mgr.submit(order).unwrap();
            assert_eq!(id.len(), 36, "UUID should be 36 chars, got: {id}");
            assert_eq!(id.chars().filter(|c| *c == '-').count(), 4);
            assert_eq!(mgr.active_count(), 1);
            assert_eq!(mgr.history_count(), 1);
        });
    }

    /// `cancel` 后订单从 active 移除,进入 history
    #[test]
    fn order_manager_cancel_removes_from_active() {
        Python::attach(|py| {
            let mgr = PyOrderManager::new();
            let order = build_order(py, None);
            let id = mgr.submit(order).unwrap();
            mgr.update_status(
                &id,
                PyOrderStatus {
                    kind: "Acknowledged".into(),
                    filled_qty: None,
                    avg_price: None,
                    reason: None,
                },
            )
            .unwrap();
            mgr.cancel(&id).unwrap();
            assert_eq!(mgr.active_count(), 0);
            assert_eq!(mgr.history_count(), 1);
            // 重复 cancel 触发 OrderNotFound
            let err = mgr.cancel(&id).unwrap_err();
            assert!(err.to_string().contains("OrderNotFound"));
        });
    }

    /// `update_status` 走完状态机 New → Submitted → Acknowledged → Filled
    #[test]
    fn order_manager_update_status_full_lifecycle() {
        Python::attach(|py| {
            let mgr = PyOrderManager::new();
            let order = build_order(py, None);
            let id = mgr.submit(order).unwrap();
            // submit 已把状态推到 Submitted;get 出来确认
            let s = mgr.get_order_status(&id).unwrap().unwrap();
            assert_eq!(s.kind, "Submitted");

            mgr.update_status(
                &id,
                PyOrderStatus {
                    kind: "Acknowledged".into(),
                    filled_qty: None,
                    avg_price: None,
                    reason: None,
                },
            )
            .unwrap();
            let s = mgr.get_order_status(&id).unwrap().unwrap();
            assert_eq!(s.kind, "Acknowledged");
        });
    }

    /// `update_status` 无效 UUID → `PyValueError`(不是 OmsError)
    #[test]
    fn order_manager_update_status_invalid_id_raises_value_error() {
        Python::attach(|py| {
            let mgr = PyOrderManager::new();
            let result = mgr.update_status(
                "not-a-uuid",
                PyOrderStatus {
                    kind: "Acknowledged".into(),
                    filled_qty: None,
                    avg_price: None,
                    reason: None,
                },
            );
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyValueError>(py));
        });
    }

    /// `update_status` 不存在的 UUID → OmsError(OrderNotFound)
    #[test]
    fn order_manager_update_status_missing_id_raises_oms_error() {
        Python::attach(|_py| {
            let mgr = PyOrderManager::new();
            let missing = "00000000-0000-0000-0000-000000000000";
            let result = mgr.update_status(
                missing,
                PyOrderStatus {
                    kind: "Acknowledged".into(),
                    filled_qty: None,
                    avg_price: None,
                    reason: None,
                },
            );
            assert!(result.is_err());
            let err_str = result.unwrap_err().to_string();
            assert!(err_str.contains("[OrderNotFound]"), "got: {err_str}");
        });
    }

    /// `batch_submit` 一次提交多单
    #[test]
    fn order_manager_batch_submit_returns_unique_ids() {
        Python::attach(|py| {
            let mgr = PyOrderManager::new();
            let orders: Vec<PyOrder> = (0..5).map(|_| build_order(py, None)).collect();
            let ids = mgr.batch_submit(orders).unwrap();
            assert_eq!(ids.len(), 5);
            // UUID 唯一
            let unique: std::collections::HashSet<&String> = ids.iter().collect();
            assert_eq!(unique.len(), 5);
            assert_eq!(mgr.active_count(), 5);
        });
    }

    /// `batch_submit` 重复 idempotency_key → OmsError
    #[test]
    fn order_manager_batch_submit_duplicate_idempotency_raises() {
        Python::attach(|py| {
            let mgr = PyOrderManager::new();
            let o1 = build_order(py, Some("dup-key"));
            let o2 = build_order(py, Some("dup-key"));
            let result = mgr.batch_submit(vec![o1, o2]);
            assert!(result.is_err());
            let err_str = result.unwrap_err().to_string();
            assert!(
                err_str.contains("[DuplicateIdempotencyKey]"),
                "got: {err_str}"
            );
            // 第 1 单已提交
            assert_eq!(mgr.active_count(), 1);
        });
    }

    /// `snapshot` 包含 version + active_orders + history_count
    #[test]
    fn order_manager_snapshot_structure() {
        Python::attach(|py| {
            let mgr = PyOrderManager::new();
            let order = build_order(py, None);
            let id = mgr.submit(order).unwrap();

            let snap = mgr.snapshot(py).unwrap();
            let version: u64 = snap
                .get_item("version")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(version, 1);

            let active: Bound<'_, PyDict> = snap
                .get_item("active_orders")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(active.len(), 1);
            assert!(active.contains(id).unwrap());

            let history_count: usize = snap
                .get_item("history_count")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(history_count, 1);
        });
    }
}
