//! `L2MatchingEngine` + `MatchingStats` Python 绑定
//!
//! 暴露 L2 撮合引擎(在 L1 基础上增加修改 / O(1) 取消 / 统计 / 订单簿导入导出)
//! 到 Python。
//!
//! # 与 L1 的差异
//!
//! - `modify(order_id, new_price, new_quantity)` 修改订单,价格变化时
//!   重新排序到新价位末尾(同价位内 FIFO),数量变化校验不小于已成交量。
//! - `stats` 属性返回 `MatchingStats` 的 dict 表示(total_fills / total_volume /
//!   total_turnover / matched_orders)。
//! - `volume_at_price(side, price)` 查询指定价位挂单量。
//! - `from_entries(entries)` / `export_entries()` 订单簿导入导出(快照恢复用)。
//!
//! # 错误处理
//!
//! - `modify` 失败(order 不存在 / 价格非法 / 数量非法)走 `MatchingError` →
//!   `BacktestError`(`code="Matching"`)路径。详见 `super::error`。
//! - `submit` 失败行为与 L1 一致(返回 `SubmitResult::empty()`,见
//!   `super::matching_l1` 注释)。

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use axon_core::market::Side as CoreSide;
use axon_core::types::{Price, Quantity, Symbol};

use crate::matching::l2::{
    L2MatchingEngine as RustL2Engine, MatchingStats as RustMatchingStats,
    OrderBookEntry as RustOrderBookEntry, OrderLocation as RustOrderLocation,
};
use crate::matching::types::OrderBookLevel;

use super::error::to_py_err;
use super::types::{dict_to_order, submit_result_to_dict};

/// Python 侧 L2 撮合引擎
#[pyclass(name = "L2MatchingEngine")]
pub struct PyL2MatchingEngine {
    inner: RustL2Engine,
}

#[pymethods]
impl PyL2MatchingEngine {
    /// 创建 L2 撮合引擎。
    ///
    /// Args:
    /// - `symbol`:可选,绑定交易品种
    #[new]
    #[pyo3(signature = (symbol=None))]
    fn new(symbol: Option<String>) -> Self {
        let inner = match symbol {
            Some(s) => RustL2Engine::with_symbol(Symbol::from(s)),
            None => RustL2Engine::new(),
        };
        Self { inner }
    }

    /// 提交订单(Python dict → Rust Order),返回成交结果 dict。
    fn submit<'py>(
        &mut self,
        py: Python<'py>,
        order_dict: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let order = dict_to_order(order_dict)?;
        let result = self.inner.submit(order);
        submit_result_to_dict(py, &result)
    }

    /// 取消订单。
    fn cancel(&mut self, order_id: u64) -> bool {
        self.inner.cancel(order_id)
    }

    /// 查询订单是否存在(仅活跃)
    fn contains(&self, order_id: u64) -> bool {
        self.inner.contains(order_id)
    }

    /// 查询订单在订单簿中的位置
    ///
    /// Returns:dict 含 `side` / `price` / `offset`,或 `None`(订单不存在)
    fn location<'py>(&self, py: Python<'py>, order_id: u64) -> Option<Bound<'py, PyDict>> {
        self.inner
            .location(order_id)
            .map(|loc| location_to_dict(py, loc).expect("PyDict::new 不应失败"))
    }

    /// 修改订单。
    ///
    /// Args:
    /// - `order_id`:目标订单 ID
    /// - `new_price`:新价格(`None` 表示不修改)
    /// - `new_quantity`:新数量(`None` 表示不修改)
    ///
    /// 失败时抛 `BacktestError(code="Matching")`。
    #[pyo3(signature = (order_id, new_price=None, new_quantity=None))]
    fn modify(
        &mut self,
        order_id: u64,
        new_price: Option<f64>,
        new_quantity: Option<f64>,
    ) -> PyResult<()> {
        let new_price = new_price.map(Price::from_f64);
        let new_quantity = new_quantity.map(Quantity::from_f64);
        // 用 `map_err(|e| to_py_err(e.into()))` 把 MatchingError → BacktestError,
        // 避免 `?` 自动转换(没有 `From<MatchingError> for PyErr` 实现)。
        // 注意:`to_py_err` 接 `BacktestErrorKind`,所以这里要把 `e: MatchingError`
        // 用 `.into()` 升到 `BacktestErrorKind::Matching(_)`。
        self.inner
            .modify(order_id, new_price, new_quantity)
            .map_err(|e| to_py_err(e.into()))?;
        Ok(())
    }

    /// 查询指定价位的挂单量
    fn volume_at_price(&self, side: &str, price: f64) -> PyResult<f64> {
        let s = super::super::python::types::parse_side(side)?;
        Ok(self
            .inner
            .volume_at_price(s, Price::from_f64(price))
            .as_f64())
    }

    /// 订单簿深度快照(dict 含 `bids` / `asks`)
    #[pyo3(signature = (levels=10))]
    fn depth<'py>(&self, py: Python<'py>, levels: usize) -> PyResult<Bound<'py, PyDict>> {
        let (bids, asks) = self.inner.depth(levels);
        let d = PyDict::new(py);
        d.set_item("bids", levels_to_pylist(py, &bids)?)?;
        d.set_item("asks", levels_to_pylist(py, &asks)?)?;
        Ok(d)
    }

    /// 最优买价
    #[getter]
    fn best_bid(&self) -> Option<f64> {
        self.inner.best_bid().map(|p| p.as_f64())
    }

    /// 最优卖价
    #[getter]
    fn best_ask(&self) -> Option<f64> {
        self.inner.best_ask().map(|p| p.as_f64())
    }

    /// 买卖价差
    #[getter]
    fn spread(&self) -> Option<f64> {
        self.inner.spread().map(|p| p.as_f64())
    }

    /// 当前活跃订单数
    #[getter]
    fn active_order_count(&self) -> usize {
        self.inner.active_order_count()
    }

    /// 累计统计(MatchingStats 快照)
    #[getter]
    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        stats_to_dict(py, self.inner.stats())
    }

    /// 从条目列表恢复订单簿(快照恢复场景)
    ///
    /// 实现注:不用 `Vec<PyOrderBookEntry>` 直接作参数是因为 `#[pyclass(skip_from_py_object)]`
    /// 阻断了 pyo3 的 `FromPyObjectOwned` 自动实现(我们主动 skip 了,只走显式
    /// `extract` 路径)。这里接收 `&Bound<'py, PyList>` 手动遍历 + `extract::<PyOrderBookEntry>()`。
    #[staticmethod]
    fn from_entries<'py>(entries: &Bound<'py, PyList>) -> PyResult<Self> {
        let mut rust_entries = Vec::with_capacity(entries.len());
        for item in entries.iter() {
            let entry = item.extract::<PyOrderBookEntry>()?;
            rust_entries.push(entry.into_rust());
        }
        Ok(Self {
            inner: RustL2Engine::from_entries(rust_entries),
        })
    }

    /// 导出当前订单簿为条目列表
    fn export_entries<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let entries = self.inner.export_entries();
        let list = PyList::empty(py);
        for e in entries {
            let d = PyDict::new(py);
            d.set_item("order_id", e.order_id)?;
            d.set_item("side", format!("{}", e.side))?;
            d.set_item("price", e.price.as_f64())?;
            d.set_item("quantity", e.quantity.as_f64())?;
            d.set_item("filled_quantity", e.filled_quantity.as_f64())?;
            list.append(d)?;
        }
        Ok(list)
    }

    fn __repr__(&self) -> String {
        let stats = self.inner.stats();
        format!(
            "L2MatchingEngine(active_orders={}, total_fills={}, best_bid={:?}, best_ask={:?})",
            self.inner.active_order_count(),
            stats.total_fills,
            self.inner.best_bid().map(|p| p.as_f64()),
            self.inner.best_ask().map(|p| p.as_f64()),
        )
    }
}

// ─── OrderBookEntry 包装(用于 from_entries) ───────────────────

/// Python 端 `OrderBookEntry` 包装
///
/// 用途:从 Python 构造 `OrderBookEntry` 列表传入 `L2MatchingEngine.from_entries()`。
/// 不能直接暴露 Rust `OrderBookEntry`(`RejectReason` 等复杂字段不易跨语言序列化),
/// 这里用纯 dict-like 接口。
///
/// 派生:`#[pyclass(from_py_object)]` + `#[derive(Clone)]` 让 pyo3 0.28 的
/// `FromPyObject` 自动实现走 `ExtractPyClassWithClone` 路径,从而支持
/// `extract::<PyOrderBookEntry>()`(消除 `HasAutomaticFromPyObject` 弃用警告)。
#[pyclass(name = "OrderBookEntry", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyOrderBookEntry {
    order_id: u64,
    side_str: String,
    price: f64,
    quantity: f64,
    filled_quantity: f64,
}

#[pymethods]
impl PyOrderBookEntry {
    #[new]
    fn new(
        order_id: u64,
        side: &str,
        price: f64,
        quantity: f64,
        filled_quantity: f64,
    ) -> PyResult<Self> {
        // 解析 side(用 types::parse_side 做大小写不敏感校验)
        let _ = super::super::python::types::parse_side(side)?;
        Ok(Self {
            order_id,
            side_str: side.to_lowercase(),
            filled_quantity,
            price,
            quantity,
        })
    }

    #[getter]
    fn order_id(&self) -> u64 {
        self.order_id
    }

    #[getter]
    fn side(&self) -> &str {
        &self.side_str
    }

    #[getter]
    fn price(&self) -> f64 {
        self.price
    }

    #[getter]
    fn quantity(&self) -> f64 {
        self.quantity
    }

    #[getter]
    fn filled_quantity(&self) -> f64 {
        self.filled_quantity
    }

    fn __repr__(&self) -> String {
        format!(
            "OrderBookEntry(id={}, side={}, price={}, quantity={}, filled={})",
            self.order_id, self.side_str, self.price, self.quantity, self.filled_quantity
        )
    }
}

impl PyOrderBookEntry {
    /// 转 Rust `OrderBookEntry`
    ///
    /// 注:`reject_reason` 设为 `Other`(简化处理,`from_entries` 路径不涉及拒单)。
    fn into_rust(self) -> RustOrderBookEntry {
        let side = match self.side_str.as_str() {
            "buy" => CoreSide::Buy,
            "sell" => CoreSide::Sell,
            _ => unreachable!("`new` 已校验过 side 合法性"),
        };
        RustOrderBookEntry {
            order_id: self.order_id,
            side,
            price: Price::from_f64(self.price),
            quantity: Quantity::from_f64(self.quantity),
            filled_quantity: Quantity::from_f64(self.filled_quantity),
        }
    }
}

// ─── 辅助函数 ────────────────────────────────────────────

/// `OrderLocation` → Python dict
fn location_to_dict<'py>(py: Python<'py>, loc: &RustOrderLocation) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("side", format!("{}", loc.side))?;
    d.set_item("price", loc.price.as_f64())?;
    d.set_item("offset", loc.offset)?;
    Ok(d)
}

/// `MatchingStats` → Python dict
fn stats_to_dict<'py>(py: Python<'py>, stats: &RustMatchingStats) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("total_fills", stats.total_fills)?;
    d.set_item("total_volume", stats.total_volume)?;
    d.set_item("total_turnover", stats.total_turnover)?;
    d.set_item("matched_orders", stats.matched_orders)?;
    Ok(d)
}

/// `Vec<OrderBookLevel>` → Python `list[dict]`
fn levels_to_pylist<'py>(
    py: Python<'py>,
    levels: &[OrderBookLevel],
) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for lvl in levels {
        let d = PyDict::new(py);
        d.set_item("price", lvl.price.as_f64())?;
        d.set_item("quantity", lvl.quantity.as_f64())?;
        d.set_item("order_count", lvl.order_count)?;
        list.append(d)?;
    }
    Ok(list)
}

/// 在 `_native.backtest` 子模块下注册 `L2MatchingEngine` + `OrderBookEntry`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyL2MatchingEngine>()?;
    parent.add_class::<PyOrderBookEntry>()?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyDict;

    fn make_limit_dict<'py>(
        py: Python<'py>,
        id: u64,
        side: &str,
        price: f64,
        qty: f64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("id", id)?;
        d.set_item("symbol", "BTC-USDT")?;
        d.set_item("side", side)?;
        d.set_item("type", "limit")?;
        d.set_item("price", price)?;
        d.set_item("quantity", qty)?;
        d.set_item("tif", "GTC")?;
        Ok(d)
    }

    /// `__repr__` 含 L2 关键字段
    #[test]
    fn repr_includes_l2_fields() {
        let e = PyL2MatchingEngine::new(None);
        let s = e.__repr__();
        assert!(s.contains("L2MatchingEngine"));
        assert!(s.contains("total_fills=0"));
    }

    /// 空 L2:stats 全 0,depth 空
    #[test]
    fn empty_l2_defaults() {
        Python::attach(|py| {
            let e = PyL2MatchingEngine::new(None);
            assert_eq!(e.active_order_count(), 0);
            let stats = e.stats(py).unwrap();
            assert_eq!(
                stats
                    .get_item("total_fills")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                0
            );
            assert!(e.best_bid().is_none());
        });
    }

    /// `submit` 卖单挂单后,`volume_at_price("sell", 100)` 返回 1.0
    #[test]
    fn volume_at_price_after_submit() {
        Python::attach(|py| {
            let mut e = PyL2MatchingEngine::new(None);
            let d = make_limit_dict(py, 1, "sell", 100.0, 1.0).unwrap();
            e.submit(py, &d).unwrap();
            let v = e.volume_at_price("sell", 100.0).unwrap();
            assert!((v - 1.0).abs() < 1e-9);
        });
    }

    /// `stats` 在 fill 后累加
    #[test]
    fn stats_increments_after_fill() {
        Python::attach(|py| {
            let mut e = PyL2MatchingEngine::new(None);
            // 卖单挂单
            let sell = make_limit_dict(py, 1, "sell", 100.0, 2.0).unwrap();
            e.submit(py, &sell).unwrap();
            // 买单吃 1.0
            let buy = make_limit_dict(py, 2, "buy", 100.0, 1.0).unwrap();
            e.submit(py, &buy).unwrap();
            let stats = e.stats(py).unwrap();
            assert_eq!(
                stats
                    .get_item("total_fills")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                1
            );
            assert!(
                stats
                    .get_item("total_volume")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap()
                    > 0
            );
            assert!(
                stats
                    .get_item("total_turnover")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap()
                    > 0
            );
        });
    }

    /// `modify` 不存在订单 → `BacktestError`
    #[test]
    fn modify_nonexistent_order_raises_backtest_error() {
        Python::attach(|_py| {
            let mut e = PyL2MatchingEngine::new(None);
            let err = e.modify(999, Some(100.0), None).unwrap_err();
            let s = err.to_string();
            assert!(s.contains("[Matching]"), "expected [Matching], got: {s}");
        });
    }

    /// `from_entries` → `export_entries` 往返保序
    #[test]
    fn from_entries_roundtrip() {
        Python::attach(|py| {
            let list = PyList::empty(py);
            list.append(PyOrderBookEntry::new(1, "buy", 99.0, 5.0, 0.0).unwrap())
                .unwrap();
            list.append(PyOrderBookEntry::new(2, "sell", 101.0, 3.0, 0.0).unwrap())
                .unwrap();
            let e = PyL2MatchingEngine::from_entries(&list).unwrap();
            let exported = e.export_entries(py).unwrap();
            assert_eq!(exported.len(), 2);
        });
    }

    /// `OrderBookEntry.__repr__` 含关键字段
    #[test]
    fn order_book_entry_repr() {
        let e = PyOrderBookEntry::new(1, "BUY", 100.0, 5.0, 1.0).unwrap();
        let s = e.__repr__();
        assert!(s.contains("OrderBookEntry"));
        assert!(s.contains("id=1"));
        assert!(s.contains("side=buy")); // 大小写不敏感,统一 lowercase
    }

    /// `OrderBookEntry` 非法 side 返回 `PyValueError`
    #[test]
    fn order_book_entry_invalid_side() {
        Python::attach(|py| {
            let err = PyOrderBookEntry::new(1, "xxx", 100.0, 1.0, 0.0).unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyValueError>(py));
        });
    }

    /// `register` 函数签名稳定(编译期断言)
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
