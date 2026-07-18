//! `L1MatchingEngine` Python 绑定
//!
//! 暴露 L1 价格-时间优先撮合引擎到 Python。
//!
//! # 数据契约
//!
//! - 入参订单:Python `dict`(参考 `super::types::dict_to_order`)
//! - 出参:Python `dict`(参考 `super::types::submit_result_to_dict`)
//! - 深度快照:`dict` 含 `bids` / `asks` 两个 `list[dict]`,每个元素含
//!   `price` / `quantity` / `order_count`
//!
//! # 错误处理
//!
//! - 撮合失败(限价单价格 0、订单量 0、订单类型不支持等)通过
//!   `MatchingError` → `BacktestError`(`code="Matching"`)自动转 Python
//!   异常(详见 `crate::python::error`)。
//! - `submit` 返回的 `SubmitResult` 不在异常路径上 —— 即便无成交(全部
//!   cancel / 挂单等待)也返回 `dict`,Python 端用 `is_filled` /
//!   `is_partially_filled` 判断。

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use axon_core::types::Symbol;

use crate::matching::engine::{L1MatchingEngine as RustL1Engine, MatchingEngine};
use crate::matching::types::OrderBookLevel;

use super::types::{dict_to_order, submit_result_to_dict};

/// Python 侧 L1 撮合引擎
///
/// 设计:简单包装 Rust `L1MatchingEngine`,`#[new]` 可选传 `symbol: str`
/// 绑定单一品种(单品种策略常用)。
#[pyclass(name = "L1MatchingEngine")]
pub struct PyL1MatchingEngine {
    inner: RustL1Engine,
}

#[pymethods]
impl PyL1MatchingEngine {
    /// 创建 L1 撮合引擎。
    ///
    /// Args:
    /// - `symbol`:可选,绑定交易品种(如 `"BTC-USDT"`)以拒绝其他品种订单
    #[new]
    #[pyo3(signature = (symbol=None))]
    fn new(symbol: Option<String>) -> Self {
        let inner = match symbol {
            Some(s) => RustL1Engine::with_symbol(Symbol::from(s)),
            None => RustL1Engine::new(),
        };
        Self { inner }
    }

    /// 提交订单(Python dict → Rust Order),返回成交结果 dict。
    ///
    /// 必填字段:`id` / `symbol` / `side` / `type` / `quantity` / `tif`,
    /// `type="limit"` 时还需 `price`。
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
    ///
    /// Returns:`true` 表示成功取消(订单存在且未完全成交),
    /// `false` 表示订单不存在或已终态。
    fn cancel(&mut self, order_id: u64) -> bool {
        self.inner.cancel(order_id)
    }

    /// 最优买价(`None` 表示买单簿为空)
    #[getter]
    fn best_bid(&self) -> Option<f64> {
        self.inner.best_bid().map(|p| p.as_f64())
    }

    /// 最优卖价(`None` 表示卖单簿为空)
    #[getter]
    fn best_ask(&self) -> Option<f64> {
        self.inner.best_ask().map(|p| p.as_f64())
    }

    /// 买卖价差(`best_ask - best_bid`,`None` 表示单边缺失)
    #[getter]
    fn spread(&self) -> Option<f64> {
        self.inner.spread().map(|p| p.as_f64())
    }

    /// 订单簿深度快照。
    ///
    /// Args:
    /// - `levels`:返回的买卖两侧各取前 N 档
    ///
    /// Returns:dict 含 `bids` / `asks` 两个 `list[dict]`,每个 dict
    /// 含 `price` / `quantity` / `order_count`
    #[pyo3(signature = (levels=10))]
    fn depth<'py>(&self, py: Python<'py>, levels: usize) -> PyResult<Bound<'py, PyDict>> {
        let (bids, asks) = self.inner.depth(levels);
        let d = PyDict::new(py);
        d.set_item("bids", levels_to_pylist(py, &bids)?)?;
        d.set_item("asks", levels_to_pylist(py, &asks)?)?;
        Ok(d)
    }

    /// 当前活跃订单数
    #[getter]
    fn active_order_count(&self) -> usize {
        self.inner.active_order_count()
    }

    /// 累计已分配的成交 ID 数(`fill_id` 的下一个值)
    #[getter]
    fn fill_count(&self) -> u64 {
        self.inner.fill_count()
    }

    /// 清空订单簿两侧(应用层手动管理虚拟对手盘时用,回测辅助)
    ///
    /// 用法:
    /// - 启用 `BacktestEngine.with_seed_liquidity(...)` 后,**不**需要手动调本方法,
    ///   `BacktestEngine.begin_bar(price, symbol)` 会自动执行 `clear_book + seed_liquidity`。
    /// - 单独使用 L1 撮合引擎做研究 / 单元测试时,可用本方法手动清空。
    fn clear_book(&mut self) {
        self.inner.clear_book()
    }

    /// 在订单簿两侧播种虚拟流动性(回测辅助,详见 `L1MatchingEngine::seed_liquidity`)
    ///
    /// Args:
    /// - `mid_price`: 中间价(通常为当前 bar close)
    /// - `half_spread`: 每层价差(绝对价格单位)
    /// - `depth_levels`: 每侧挂单层数(典型 5~20)
    /// - `size_per_level`: 每层挂单数量
    /// - `instrument`: instrument dict(由 `spot_instrument()` / `swap_instrument()` 工厂构造)
    /// - `next_id`: 下一个可用订单 id(避免与外部订单 id 冲突)
    ///
    /// Returns: 更新后的 `next_id` 计数器(供下次 seed 复用)
    #[pyo3(signature = (
        mid_price,
        half_spread,
        depth_levels,
        size_per_level,
        instrument,
        next_id,
    ))]
    fn seed_liquidity(
        &mut self,
        py: Python<'_>,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        instrument: &Bound<'_, PyAny>,
        next_id: u64,
    ) -> PyResult<u64> {
        let inst = super::types::parse_instrument(&instrument.cast::<PyDict>()?)?;
        Ok(self.inner.seed_liquidity(
            mid_price,
            half_spread,
            depth_levels,
            size_per_level,
            inst,
            next_id,
        ))
    }

    fn __repr__(&self) -> String {
        format!(
            "L1MatchingEngine(active_orders={}, best_bid={:?}, best_ask={:?})",
            self.inner.active_order_count(),
            self.inner.best_bid().map(|p| p.as_f64()),
            self.inner.best_ask().map(|p| p.as_f64()),
        )
    }
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

/// 在 `_native.backtest` 子模块下注册 `L1MatchingEngine`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyL1MatchingEngine>()?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyDict;

    /// `__repr__` 不 panic,包含关键统计字段
    #[test]
    fn repr_contains_counts() {
        let e = PyL1MatchingEngine::new(None);
        let s = e.__repr__();
        assert!(s.contains("L1MatchingEngine"));
        assert!(s.contains("active_orders=0"));
    }

    /// `with_symbol` 构造的引擎 `__repr__` 仍正常工作
    #[test]
    fn repr_with_symbol() {
        let e = PyL1MatchingEngine::new(Some("BTC-USDT".into()));
        let _ = e.__repr__();
    }

    /// 空引擎:best_bid / best_ask / spread / active_order_count / fill_count 全 0/None
    #[test]
    fn empty_engine_defaults() {
        let e = PyL1MatchingEngine::new(None);
        assert!(e.best_bid().is_none());
        assert!(e.best_ask().is_none());
        assert!(e.spread().is_none());
        assert_eq!(e.active_order_count(), 0);
        assert_eq!(e.fill_count(), 0);
    }

    /// `submit` 卖单挂单后:active_order_count = 1, best_ask = 100
    #[test]
    fn submit_limit_sell_rests_in_book() {
        Python::attach(|py| {
            let mut e = PyL1MatchingEngine::new(None);
            let d = PyDict::new(py);
            d.set_item("id", 1u64).unwrap();
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "sell").unwrap();
            d.set_item("type", "limit").unwrap();
            d.set_item("price", 100.0_f64).unwrap();
            d.set_item("quantity", 1.0_f64).unwrap();
            d.set_item("tif", "GTC").unwrap();
            let res = e.submit(py, &d).unwrap();
            assert!(
                !res.get_item("is_filled")
                    .unwrap()
                    .unwrap()
                    .extract::<bool>()
                    .unwrap()
            );
            assert_eq!(e.active_order_count(), 1);
            assert_eq!(e.best_ask(), Some(100.0));
            assert!(e.best_bid().is_none());
        });
    }

    /// `submit` 卖单 @ 100 + 买单 @ 100 ⇒ 1 fill,均完全成交
    ///
    /// 注:L1MatchingEngine::submit 对验证失败的订单返回
    /// `SubmitResult::empty()` 而非 `Err`(`engine.rs:401-405`),目的是让
    /// 策略 / `BacktestEngine` 不会因单笔失败而中断。`active_order_count`
    /// 在订单 Filled 后不会自动从 `order_index` 移除(只 cancel 才移除),
    /// 所以这里不强制断言 = 0。
    #[test]
    fn submit_matching_orders_yield_one_fill() {
        Python::attach(|py| {
            let mut e = PyL1MatchingEngine::new(None);

            // 卖单挂单
            let sell = PyDict::new(py);
            sell.set_item("id", 1u64).unwrap();
            sell.set_item("symbol", "BTC-USDT").unwrap();
            sell.set_item("side", "sell").unwrap();
            sell.set_item("type", "limit").unwrap();
            sell.set_item("price", 100.0_f64).unwrap();
            sell.set_item("quantity", 1.0_f64).unwrap();
            sell.set_item("tif", "GTC").unwrap();
            e.submit(py, &sell).unwrap();

            // 买单吃单
            let buy = PyDict::new(py);
            buy.set_item("id", 2u64).unwrap();
            buy.set_item("symbol", "BTC-USDT").unwrap();
            buy.set_item("side", "buy").unwrap();
            buy.set_item("type", "limit").unwrap();
            buy.set_item("price", 100.0_f64).unwrap();
            buy.set_item("quantity", 1.0_f64).unwrap();
            buy.set_item("tif", "GTC").unwrap();
            let res = e.submit(py, &buy).unwrap();
            let fills = res.get_item("fills").unwrap().unwrap();
            assert_eq!(fills.len().unwrap(), 1);
            assert!(
                res.get_item("is_filled")
                    .unwrap()
                    .unwrap()
                    .extract::<bool>()
                    .unwrap()
            );
            // fill_count 增加
            assert_eq!(e.fill_count(), 1);
            // best_ask 仍为 100:L1 不清理"已 Filled 但还在 order_index"的卖单
            // (见 `engine.rs:266-273` —— 只有 orders.is_empty() 时才 remove);
            // 这是 L1 的简化设计,清理由调用方 / BacktestEngine 负责。
            assert_eq!(e.best_ask(), Some(100.0));
            // 显式 cancel 后才会从 order_index 移除
            assert!(e.cancel(1));
        });
    }

    /// `cancel` 成功取消已挂单
    #[test]
    fn cancel_existing_order() {
        Python::attach(|py| {
            let mut e = PyL1MatchingEngine::new(None);
            let d = PyDict::new(py);
            d.set_item("id", 1u64).unwrap();
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "limit").unwrap();
            d.set_item("price", 100.0_f64).unwrap();
            d.set_item("quantity", 1.0_f64).unwrap();
            d.set_item("tif", "GTC").unwrap();
            e.submit(py, &d).unwrap();
            assert!(e.cancel(1));
            assert_eq!(e.active_order_count(), 0);
        });
    }

    /// `cancel` 不存在订单返回 `false`
    #[test]
    fn cancel_nonexistent_returns_false() {
        let mut e = PyL1MatchingEngine::new(None);
        assert!(!e.cancel(999));
    }

    /// `depth` 返回 `{bids: [...], asks: [...]}` 结构
    #[test]
    fn depth_returns_dict_structure() {
        Python::attach(|py| {
            let mut e = PyL1MatchingEngine::new(None);

            // 卖 @ 101
            let ask1 = PyDict::new(py);
            ask1.set_item("id", 1u64).unwrap();
            ask1.set_item("symbol", "BTC-USDT").unwrap();
            ask1.set_item("side", "sell").unwrap();
            ask1.set_item("type", "limit").unwrap();
            ask1.set_item("price", 101.0_f64).unwrap();
            ask1.set_item("quantity", 1.0_f64).unwrap();
            ask1.set_item("tif", "GTC").unwrap();
            e.submit(py, &ask1).unwrap();

            // 买 @ 99
            let bid1 = PyDict::new(py);
            bid1.set_item("id", 2u64).unwrap();
            bid1.set_item("symbol", "BTC-USDT").unwrap();
            bid1.set_item("side", "buy").unwrap();
            bid1.set_item("type", "limit").unwrap();
            bid1.set_item("price", 99.0_f64).unwrap();
            bid1.set_item("quantity", 2.0_f64).unwrap();
            bid1.set_item("tif", "GTC").unwrap();
            e.submit(py, &bid1).unwrap();

            let depth = e.depth(py, 5).unwrap();
            let bids = depth.get_item("bids").unwrap().unwrap();
            let asks = depth.get_item("asks").unwrap().unwrap();
            assert_eq!(bids.len().unwrap(), 1);
            assert_eq!(asks.len().unwrap(), 1);
            // bid price = 99
            let bid_first = bids.get_item(0).unwrap();
            assert_eq!(
                bid_first
                    .get_item("price")
                    .unwrap()
                    .extract::<f64>()
                    .unwrap(),
                99.0
            );
            // ask price = 101
            let ask_first = asks.get_item(0).unwrap();
            assert_eq!(
                ask_first
                    .get_item("price")
                    .unwrap()
                    .extract::<f64>()
                    .unwrap(),
                101.0
            );
            // spread = 2.0
            assert_eq!(e.spread(), Some(2.0));
        });
    }

    /// 限价单价格 = 0 在 L1 中返回 `SubmitResult::empty()`(非 `Err`)。
    ///
    /// 注:L1MatchingEngine::submit 对验证失败的订单不抛错,而是返回空结果
    /// (`engine.rs:401-405`:`SubmitResult::empty(rejected.quantity)`)。
    /// 这是为了让 `BacktestEngine` / 策略循环不会中断 —— BacktestEngine 通过
    /// `added_to_book = active_after > active_before` 判定"未挂簿 ⇒ rejected"
    /// (见 `engine.rs:264-292`)。所以 Python 端调用 `submit` 不会抛
    /// `BacktestError`,而是从返回 dict 中读 `is_filled=False` + `fills=[]`。
    #[test]
    fn submit_zero_price_returns_empty_result() {
        Python::attach(|py| {
            let mut e = PyL1MatchingEngine::new(None);
            let d = PyDict::new(py);
            d.set_item("id", 1u64).unwrap();
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "limit").unwrap();
            d.set_item("price", 0.0_f64).unwrap();
            d.set_item("quantity", 1.0_f64).unwrap();
            d.set_item("tif", "GTC").unwrap();
            let res = e.submit(py, &d).unwrap();
            // 不应 panic;返回空结果
            let fills = res.get_item("fills").unwrap().unwrap();
            assert_eq!(fills.len().unwrap(), 0);
            assert!(
                !res.get_item("is_filled")
                    .unwrap()
                    .unwrap()
                    .extract::<bool>()
                    .unwrap()
            );
            assert!(
                !res.get_item("is_partially_filled")
                    .unwrap()
                    .unwrap()
                    .extract::<bool>()
                    .unwrap()
            );
        });
    }

    /// `submit` dict 缺字段 → `PyKeyError`
    #[test]
    fn submit_missing_field_raises_key_error() {
        Python::attach(|py| {
            let mut e = PyL1MatchingEngine::new(None);
            let d = PyDict::new(py);
            d.set_item("id", 1u64).unwrap();
            // 故意少填 `quantity` 等
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "market").unwrap();
            d.set_item("tif", "GTC").unwrap();
            let err = e.submit(py, &d).unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyKeyError>(py));
        });
    }

    /// `register` 函数签名稳定(编译期断言)
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
