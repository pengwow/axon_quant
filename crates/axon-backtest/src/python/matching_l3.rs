//! `MultiAssetMatchingEngine` + L3 配套类型 Python 绑定
//!
//! 暴露 L3 多资产撮合引擎到 Python,包括:
//! - 多资产路由(`register_asset` / 隐式随 `submit` 注册)
//! - 跨资产交易对配置(`register_cross_pair`)
//! - 批量撮合模式切换(`set_batch_mode`)
//! - 批量拍卖(`run_auction`)
//! - 暗池订单(`submit_dark_order`)
//! - 跨资产套利(`detect_arbitrage` / `execute_arbitrage`)
//! - 引擎快照与恢复(`snapshot` / `restore`)
//!
//! # 数据契约
//!
//! - **入参订单**:Python `dict`(参考 `super::types::dict_to_order`)
//! - **入参暗池订单**:`DarkOrder` `#[pyclass]`
//! - **入参跨资产交易对**:`CrossPair` `#[pyclass]`(支持 `dict_to_cross_pair` 自动转换)
//! - **出参**:成交列表 dict / 拍卖结果 `AuctionResult` `#[pyclass]` /
//!   套利机会列表 `[ArbitrageOpportunity]` / 统计 dict
//! - **per-symbol 子引擎**:通过 `best_bid(symbol)` / `best_ask(symbol)` /
//!   `depth(symbol, levels)` / `stats(symbol)` 等方法查询,避免 Rust
//!   借用 `HashMap<Symbol, L2MatchingEngine>` 跨 FFI 边界。
//!
//! # 错误处理
//!
//! - 撮合错误(资产未注册、跨资产配置非法、暗池数量非法等)通过
//!   `MatchingL3Error` → `BacktestError`(`code="MatchingL3"`)自动转 Python
//!   异常(详见 `super::error`)。
//! - `dict` 缺字段 → `PyKeyError`,枚举值非法 → `PyValueError`。
//!
//! # 设计要点
//!
//! - **为什么不用 `#[pyclass] L2EngineHandle`?** `MultiAssetMatchingEngine`
//!   内部用 `HashMap<Symbol, L2MatchingEngine>` 存储,跨函数保持对某个
//!   symbol 的 `&mut L2MatchingEngine` 借用极不友好(rust borrow checker
//!   不允许,`GIL` 释放后更不可控)。故采用"per-symbol 方法"模式,在
//!   `PyMultiAssetMatchingEngine` 上直接提供 `best_bid(symbol)` 等
//!   一组 method,语义等价于 "L2 子引擎的远程调用",更稳定。
//! - **`PyAuctionResult` / `PyArbitrageOpportunity` / `PyL3Stats`** 用
//!   `#[pyclass]` 暴露,允许 Python 端 `result.clearing_price` 属性访问
//!   而非 `dict` —— L3 报告类数据通常按字段读取,`pyclass` 更自然。

use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use axon_core::market::Side as CoreSide;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Price, Quantity, Symbol};

use crate::matching::l3::auction::{AuctionResult as RustAuctionResult, BatchMode};
use crate::matching::l3::dark_pool::DarkOrder as RustDarkOrder;
use crate::matching::l3::engine_l3::{
    ArbitrageOpportunity as RustArbitrageOpportunity, L3Stats as RustL3Stats,
    MultiAssetMatchingEngine as RustL3Engine,
};
use crate::matching::l3::types::{
    CrossPair as RustCrossPair, MatchingEngineSnapshot as RustMatchingEngineSnapshot,
    PriceLevel as RustPriceLevel,
};
use crate::matching::types::{MatchFill, OrderBookLevel};

use super::error::to_py_err;
use super::types::{dict_to_order, match_fill_to_dict, parse_side};

// ═══════════════════════════════════════════════════════════════════════════
// 主类: PyMultiAssetMatchingEngine
// ═══════════════════════════════════════════════════════════════════════════

/// Python 侧 L3 多资产撮合引擎
///
/// 包装 Rust `MultiAssetMatchingEngine`,提供多资产路由、批量模式、
/// 暗池、拍卖、套利等高级特性。
#[pyclass(name = "MultiAssetMatchingEngine")]
pub struct PyMultiAssetMatchingEngine {
    inner: RustL3Engine,
}

#[pymethods]
impl PyMultiAssetMatchingEngine {
    /// 创建 L3 多资产撮合引擎
    #[new]
    fn new() -> Self {
        Self {
            inner: RustL3Engine::new(),
        }
    }

    /// 注册资产(幂等)
    fn register_asset(&mut self, symbol: &str) {
        self.inner.register_asset(Symbol::from(symbol));
    }

    /// 注册跨资产交易对
    ///
    /// 自动注册两个 leg 的资产。失败(leg1 == leg2、ratio 非正等)
    /// 抛 `BacktestError(code="MatchingL3")`。
    fn register_cross_pair(&mut self, py: Python<'_>, pair: &Bound<'_, PyAny>) -> PyResult<()> {
        let pair = cross_pair_from_any(py, pair)?;
        self.inner
            .register_cross_pair(pair)
            .map_err(|e| to_py_err(e.into()))?;
        Ok(())
    }

    /// 设置批量撮合模式
    ///
    /// Args:
    /// - `mode`:字符串 `"continuous"` / `"auction"` / `"dark_pool"`
    fn set_batch_mode(&mut self, mode: &str) -> PyResult<()> {
        let m = parse_batch_mode(mode)?;
        self.inner.set_batch_mode(m);
        Ok(())
    }

    /// 当前批量撮合模式(字符串 `"continuous"` / `"auction"` / `"dark_pool"`)
    #[getter]
    fn batch_mode(&self) -> String {
        batch_mode_to_str(self.inner.batch_mode()).to_string()
    }

    /// 提交订单(根据当前 `batch_mode` 路由:Continuous 立即撮合,
    /// Auction 暂存待 `run_auction`,DarkPool 进暗池簿)
    ///
    /// Returns:`list[dict]`(成交事件列表,Auction / DarkPool 模式下
    /// 未撮合时返回 `[]`)。
    fn submit<'py>(
        &mut self,
        py: Python<'py>,
        order_dict: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyList>> {
        let order = dict_to_order(order_dict)?;
        let fills = self.inner.submit(order).map_err(|e| to_py_err(e.into()))?;
        fills_to_pylist(py, &fills)
    }

    /// 批量提交订单(连续模式下逐个撮合并聚合成交)
    fn submit_batch<'py>(
        &mut self,
        py: Python<'py>,
        orders: &Bound<'py, PyList>,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut rust_orders: Vec<Order> = Vec::with_capacity(orders.len());
        for item in orders.iter() {
            let d = item
                .cast::<PyDict>()
                .map_err(|_e| PyValueError::new_err("submit_batch expects a list of dicts"))?;
            rust_orders.push(dict_to_order(d)?);
        }
        let fills = self
            .inner
            .submit_batch(rust_orders)
            .map_err(|e| to_py_err(e.into()))?;
        fills_to_pylist(py, &fills)
    }

    /// 提交暗池订单
    ///
    /// 撮合规则:先扫暗池簿,对手方价格可交叉即成交,未成交部分存入暗池。
    fn submit_dark_order<'py>(
        &mut self,
        py: Python<'py>,
        dark: &PyDarkOrder,
    ) -> PyResult<Bound<'py, PyList>> {
        let rust_dark = dark.to_rust().map_err(|e| to_py_err(e.into()))?;
        let fills = self
            .inner
            .submit_dark_order(rust_dark)
            .map_err(|e| to_py_err(e.into()))?;
        fills_to_pylist(py, &fills)
    }

    /// 运行批量拍卖(对指定 symbol 执行清算价撮合)
    ///
    /// Auction 模式下被 `submit` 暂存的订单会被消费;其他 symbol 的暂存
    /// 订单保留。Returns:`AuctionResult` 包含 `clearing_price` /
    /// `clearing_volume` / `fills` / `unfilled_orders`。
    fn run_auction(&mut self, symbol: &str) -> PyResult<PyAuctionResult> {
        let r = self
            .inner
            .run_auction(&Symbol::from(symbol))
            .map_err(|e| to_py_err(e.into()))?;
        Ok(PyAuctionResult::from_rust(r))
    }

    /// 检测套利机会
    ///
    /// Returns:每个已注册 `CrossPair` 一个 `ArbitrageOpportunity`。
    fn detect_arbitrage<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let ops = self.inner.detect_arbitrage();
        let list = PyList::empty(py);
        for op in ops {
            list.append(PyArbitrageOpportunity::from_rust(op))?;
        }
        Ok(list)
    }

    /// 执行套利
    ///
    /// Args:
    /// - `pair`:跨资产交易对 dict 或 `CrossPair` 对象
    /// - `quantity`:执行数量
    /// - `side_leg1`:leg1 的方向 `"buy"` / `"sell"`
    fn execute_arbitrage<'py>(
        &mut self,
        py: Python<'py>,
        pair: &Bound<'_, PyAny>,
        quantity: f64,
        side_leg1: &str,
    ) -> PyResult<Bound<'py, PyList>> {
        let pair = cross_pair_from_any(py, pair)?;
        let side = parse_side(side_leg1)?;
        let fills = self
            .inner
            .execute_arbitrage(&pair, Quantity::from_f64(quantity), side)
            .map_err(|e| to_py_err(e.into()))?;
        fills_to_pylist(py, &fills)
    }

    /// 已注册资产数
    #[getter]
    fn asset_count(&self) -> usize {
        self.inner.asset_count()
    }

    /// 已注册跨资产交易对数
    #[getter]
    fn cross_pair_count(&self) -> usize {
        self.inner.cross_pair_count()
    }

    /// 累计统计
    #[getter]
    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        l3_stats_to_dict(py, self.inner.stats())
    }

    // ─── per-symbol 子引擎查询(见模块级 doc 注释) ───────────

    /// 指定 symbol 的最优买价
    fn best_bid(&self, symbol: &str) -> PyResult<Option<f64>> {
        let s = Symbol::from(symbol);
        self.inner
            .engine(&s)
            .ok_or_else(|| asset_not_found_pyerr(symbol))
            .map(|e| e.best_bid().map(|p| p.as_f64()))
    }

    /// 指定 symbol 的最优卖价
    fn best_ask(&self, symbol: &str) -> PyResult<Option<f64>> {
        let s = Symbol::from(symbol);
        self.inner
            .engine(&s)
            .ok_or_else(|| asset_not_found_pyerr(symbol))
            .map(|e| e.best_ask().map(|p| p.as_f64()))
    }

    /// 指定 symbol 的买卖价差
    fn spread(&self, symbol: &str) -> PyResult<Option<f64>> {
        let s = Symbol::from(symbol);
        self.inner
            .engine(&s)
            .ok_or_else(|| asset_not_found_pyerr(symbol))
            .map(|e| e.spread().map(|p| p.as_f64()))
    }

    /// 指定 symbol 的订单簿深度
    #[pyo3(signature = (symbol, levels=10))]
    fn depth<'py>(
        &self,
        py: Python<'py>,
        symbol: &str,
        levels: usize,
    ) -> PyResult<Bound<'py, PyDict>> {
        let s = Symbol::from(symbol);
        let engine = self
            .inner
            .engine(&s)
            .ok_or_else(|| asset_not_found_pyerr(symbol))?;
        let (bids, asks) = engine.depth(levels);
        let d = PyDict::new(py);
        d.set_item("bids", order_book_levels_to_pylist(py, &bids)?)?;
        d.set_item("asks", order_book_levels_to_pylist(py, &asks)?)?;
        Ok(d)
    }

    /// 指定 symbol 的活跃订单数
    fn active_order_count(&self, symbol: &str) -> PyResult<usize> {
        let s = Symbol::from(symbol);
        self.inner
            .engine(&s)
            .ok_or_else(|| asset_not_found_pyerr(symbol))
            .map(|e| e.active_order_count())
    }

    /// 指定 symbol 的累计成交笔数
    fn fill_count(&self, symbol: &str) -> PyResult<u64> {
        let s = Symbol::from(symbol);
        self.inner
            .engine(&s)
            .ok_or_else(|| asset_not_found_pyerr(symbol))
            .map(|e| e.stats().total_fills)
    }

    /// 指定 symbol 的撮合统计
    fn stats_of<'py>(&self, py: Python<'py>, symbol: &str) -> PyResult<Bound<'py, PyDict>> {
        let s = Symbol::from(symbol);
        let engine = self
            .inner
            .engine(&s)
            .ok_or_else(|| asset_not_found_pyerr(symbol))?;
        let stats = engine.stats();
        let d = PyDict::new(py);
        d.set_item("total_fills", stats.total_fills)?;
        d.set_item("total_volume", stats.total_volume)?;
        d.set_item("total_turnover", stats.total_turnover)?;
        d.set_item("matched_orders", stats.matched_orders)?;
        Ok(d)
    }

    /// 创建快照(dict 形式)
    ///
    /// Returns:含 `batch_mode` / `engines`(dict[symbol, L2Snapshot])/
    /// `cross_pairs` / `timestamp_ns`。
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        snapshot_to_dict(py, &self.inner.snapshot())
    }

    /// 从快照恢复(仅恢复资产注册 / 跨资产配置 / 批量模式,
    /// 价格级别不自动恢复 —— 见 Rust `restore` doc 注释)
    fn restore(&mut self, py: Python<'_>, snap: &Bound<'_, PyDict>) -> PyResult<()> {
        let rust_snap = snapshot_from_dict(py, snap)?;
        self.inner
            .restore(rust_snap)
            .map_err(|e| to_py_err(e.into()))?;
        Ok(())
    }

    fn __repr__(&self) -> String {
        format!(
            "MultiAssetMatchingEngine(assets={}, cross_pairs={}, batch_mode={})",
            self.inner.asset_count(),
            self.inner.cross_pair_count(),
            batch_mode_to_str(self.inner.batch_mode()),
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PyDarkOrder: 暗池订单
// ═══════════════════════════════════════════════════════════════════════════

/// Python 侧暗池订单
///
/// 设计:与 L2 `OrderBookEntry` 一样,`#[pyclass(from_py_object)]` 让 pyo3 0.28
/// 自动生成 `FromPyObject` 实现,允许作为 `submit_dark_order` 参数,
/// Python 端可以直接传 `DarkOrder(...)` 实例。
#[pyclass(name = "DarkOrder", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyDarkOrder {
    /// 公开可见数量(冰山订单露出部分)
    visible_quantity: f64,
    /// 隐藏总数量
    hidden_quantity: f64,
    /// 订单本体各字段
    order_id: u64,
    symbol: String,
    side_str: String,
    order_type_str: String,
    price: Option<f64>,
    quantity: f64,
    tif_str: String,
}

#[pymethods]
impl PyDarkOrder {
    /// 创建暗池订单
    ///
    /// Args:
    /// - `order_id`:订单 ID
    /// - `symbol`:交易品种
    /// - `side`:`"buy"` / `"sell"`
    /// - `order_type`:`"market"` / `"limit"`
    /// - `price`:限价单必填
    /// - `quantity`:订单总数量
    /// - `tif`:有效期
    /// - `visible_quantity`:可见数量(冰山部分)
    /// - `hidden_quantity`:隐藏总数量
    #[new]
    #[pyo3(signature = (order_id, symbol, side, order_type, quantity, tif, visible_quantity, hidden_quantity, price=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        order_id: u64,
        symbol: &str,
        side: &str,
        order_type: &str,
        quantity: f64,
        tif: &str,
        visible_quantity: f64,
        hidden_quantity: f64,
        price: Option<f64>,
    ) -> PyResult<Self> {
        // 提前校验 `side` / `tif`,沿用 `parse_side` / `parse_tif` 规则
        let _ = parse_side(side)?;
        let _ = super::types::parse_tif(tif)?;
        if order_type != "limit" && order_type != "market" {
            return Err(PyValueError::new_err(format!(
                "unsupported order type: {order_type} (only 'market' / 'limit')"
            )));
        }
        if order_type == "limit" && price.is_none() {
            return Err(PyValueError::new_err("limit order requires 'price'"));
        }
        Ok(Self {
            visible_quantity,
            hidden_quantity,
            order_id,
            symbol: symbol.to_string(),
            side_str: side.to_lowercase(),
            order_type_str: order_type.to_lowercase(),
            price,
            quantity,
            tif_str: tif.to_uppercase(),
        })
    }

    #[getter]
    fn order_id(&self) -> u64 {
        self.order_id
    }

    #[getter]
    fn symbol(&self) -> &str {
        &self.symbol
    }

    #[getter]
    fn side(&self) -> &str {
        &self.side_str
    }

    #[getter]
    fn order_type(&self) -> &str {
        &self.order_type_str
    }

    #[getter]
    fn price(&self) -> Option<f64> {
        self.price
    }

    #[getter]
    fn quantity(&self) -> f64 {
        self.quantity
    }

    #[getter]
    fn tif(&self) -> &str {
        &self.tif_str
    }

    #[getter]
    fn visible_quantity(&self) -> f64 {
        self.visible_quantity
    }

    #[getter]
    fn hidden_quantity(&self) -> f64 {
        self.hidden_quantity
    }

    fn __repr__(&self) -> String {
        format!(
            "DarkOrder(id={}, symbol={}, side={}, {} @ {:?}, qty={}, visible={}/hidden={})",
            self.order_id,
            self.symbol,
            self.side_str,
            self.order_type_str,
            self.price,
            self.quantity,
            self.visible_quantity,
            self.hidden_quantity,
        )
    }
}

impl PyDarkOrder {
    /// 转 Rust `DarkOrder`(内部 helper,用于 `submit_dark_order`)
    fn to_rust(&self) -> Result<RustDarkOrder, crate::matching::l3::error::MatchingL3Error> {
        let side = match self.side_str.as_str() {
            "buy" => CoreSide::Buy,
            "sell" => CoreSide::Sell,
            _ => unreachable!("`new` 已校验过 side 合法性"),
        };
        let order_type = match self.order_type_str.as_str() {
            "limit" => {
                let p = self.price.expect("limit 必须有 price(new 已校验)");
                OrderType::Limit {
                    price: Price::from_f64(p),
                }
            }
            "market" => OrderType::Market,
            _ => unreachable!("`new` 已校验过 order_type 合法性"),
        };
        let tif = match self.tif_str.as_str() {
            "GTC" => TimeInForce::GTC,
            "IOC" => TimeInForce::IOC,
            "FOK" => TimeInForce::FOK,
            "GFD" => TimeInForce::GFD,
            "FAK" => TimeInForce::FAK,
            _ => unreachable!("`new` 已校验过 tif 合法性"),
        };
        let order = Order::new(
            self.order_id,
            Symbol::from(self.symbol.clone()),
            side,
            order_type,
            Quantity::from_f64(self.quantity),
            tif,
        );
        // `DarkOrder::new` 会校验 `visible <= hidden`,失败返回
        // `InvalidDarkOrderQuantity` —— 把它升到 `MatchingL3Error`
        // 让调用方走统一 `to_py_err` 路径。
        RustDarkOrder::new(
            order,
            Quantity::from_f64(self.visible_quantity),
            Quantity::from_f64(self.hidden_quantity),
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PyCrossPair: 跨资产交易对
// ═══════════════════════════════════════════════════════════════════════════

/// Python 侧跨资产交易对
///
/// `#[pyclass(from_py_object)]` 让 pyo3 0.28 自动生成 `FromPyObject`,
/// 同时支持 `register_cross_pair` 接受 `CrossPair(...)` 实例。
#[pyclass(name = "CrossPair", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyCrossPair {
    leg1: String,
    leg2: String,
    ratio: f64,
    max_quantity: f64,
}

#[pymethods]
impl PyCrossPair {
    /// 创建跨资产交易对
    #[new]
    fn new(leg1: &str, leg2: &str, ratio: f64, max_quantity: f64) -> Self {
        Self {
            leg1: leg1.to_string(),
            leg2: leg2.to_string(),
            ratio,
            max_quantity,
        }
    }

    #[getter]
    fn leg1(&self) -> &str {
        &self.leg1
    }

    #[getter]
    fn leg2(&self) -> &str {
        &self.leg2
    }

    #[getter]
    fn ratio(&self) -> f64 {
        self.ratio
    }

    #[getter]
    fn max_quantity(&self) -> f64 {
        self.max_quantity
    }

    fn __repr__(&self) -> String {
        format!(
            "CrossPair({}/{}, ratio={}, max_qty={})",
            self.leg1, self.leg2, self.ratio, self.max_quantity
        )
    }
}

impl PyCrossPair {
    /// 转 Rust `CrossPair`
    fn to_rust(&self) -> RustCrossPair {
        RustCrossPair::new(
            Symbol::from(self.leg1.clone()),
            Symbol::from(self.leg2.clone()),
            self.ratio,
            Quantity::from_f64(self.max_quantity),
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PyAuctionResult: 拍卖结果
// ═══════════════════════════════════════════════════════════════════════════

/// Python 侧拍卖结果
#[pyclass(name = "AuctionResult")]
pub struct PyAuctionResult {
    clearing_price: f64,
    clearing_volume: f64,
    fills: Vec<(u64, u64, u64, f64, f64, String)>,
    unfilled_count: usize,
}

#[pymethods]
impl PyAuctionResult {
    /// 清算价格(`0.0` 表示无成交)
    #[getter]
    fn clearing_price(&self) -> f64 {
        self.clearing_price
    }

    /// 清算成交量
    #[getter]
    fn clearing_volume(&self) -> f64 {
        self.clearing_volume
    }

    /// 成交事件列表(`list[dict]`)
    #[getter]
    fn fills<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for (fill_id, taker, maker, price, qty, side) in &self.fills {
            let d = PyDict::new(py);
            d.set_item("fill_id", *fill_id)?;
            d.set_item("taker_order_id", *taker)?;
            d.set_item("maker_order_id", *maker)?;
            d.set_item("price", *price)?;
            d.set_item("quantity", *qty)?;
            d.set_item("taker_side", side.clone())?;
            list.append(d)?;
        }
        Ok(list)
    }

    /// 未成交订单数量
    #[getter]
    fn unfilled_order_count(&self) -> usize {
        self.unfilled_count
    }

    /// 是否有成交(`clearing_volume > 0`)
    fn has_trades(&self) -> bool {
        self.clearing_volume > 0.0
    }

    fn __repr__(&self) -> String {
        format!(
            "AuctionResult(clearing_price={}, clearing_volume={}, fills={}, unfilled={})",
            self.clearing_price,
            self.clearing_volume,
            self.fills.len(),
            self.unfilled_count,
        )
    }
}

impl PyAuctionResult {
    /// 从 Rust `AuctionResult` 转换(预先扁平化 `fills` 避免持有
    /// `MatchFill` 跨调用)
    fn from_rust(r: RustAuctionResult) -> Self {
        let fills = r
            .fills
            .iter()
            .map(|f| {
                (
                    f.fill_id,
                    f.taker_order_id,
                    f.maker_order_id,
                    f.price.as_f64(),
                    f.quantity.as_f64(),
                    format!("{}", f.taker_side),
                )
            })
            .collect();
        Self {
            clearing_price: r.clearing_price.as_f64(),
            clearing_volume: r.clearing_volume.as_f64(),
            fills,
            unfilled_count: r.unfilled_orders.len(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PyArbitrageOpportunity: 套利机会
// ═══════════════════════════════════════════════════════════════════════════

/// Python 侧套利机会
#[pyclass(name = "ArbitrageOpportunity")]
pub struct PyArbitrageOpportunity {
    leg1: String,
    leg2: String,
    ratio: f64,
    max_quantity: f64,
    leg1_mid: Option<f64>,
    leg2_mid: Option<f64>,
    implied_ratio: Option<f64>,
    deviation: f64,
    estimated_profit: f64,
}

#[pymethods]
impl PyArbitrageOpportunity {
    #[getter]
    fn leg1(&self) -> &str {
        &self.leg1
    }

    #[getter]
    fn leg2(&self) -> &str {
        &self.leg2
    }

    #[getter]
    fn ratio(&self) -> f64 {
        self.ratio
    }

    #[getter]
    fn max_quantity(&self) -> f64 {
        self.max_quantity
    }

    #[getter]
    fn leg1_mid(&self) -> Option<f64> {
        self.leg1_mid
    }

    #[getter]
    fn leg2_mid(&self) -> Option<f64> {
        self.leg2_mid
    }

    #[getter]
    fn implied_ratio(&self) -> Option<f64> {
        self.implied_ratio
    }

    /// 偏离度(`|implied - target| / target`,`implied_ratio` 缺失时为 0)
    #[getter]
    fn deviation(&self) -> f64 {
        self.deviation
    }

    /// 估计套利利润(绝对值)
    #[getter]
    fn estimated_profit(&self) -> f64 {
        self.estimated_profit
    }

    fn __repr__(&self) -> String {
        format!(
            "ArbitrageOpportunity({}/{}, implied={:?}, deviation={}, profit={})",
            self.leg1, self.leg2, self.implied_ratio, self.deviation, self.estimated_profit
        )
    }
}

impl PyArbitrageOpportunity {
    fn from_rust(op: RustArbitrageOpportunity) -> Self {
        Self {
            leg1: op.pair.leg1.to_string(),
            leg2: op.pair.leg2.to_string(),
            ratio: op.pair.ratio,
            max_quantity: op.pair.max_quantity.as_f64(),
            leg1_mid: op.leg1_mid.map(|p| p.as_f64()),
            leg2_mid: op.leg2_mid.map(|p| p.as_f64()),
            implied_ratio: op.implied_ratio,
            deviation: op.deviation,
            estimated_profit: op.estimated_profit,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 内部辅助
// ═══════════════════════════════════════════════════════════════════════════

/// 资产未注册时构造的 `PyErr`(包装成统一 `BacktestError` 路径)
fn asset_not_found_pyerr(symbol: &str) -> PyErr {
    use crate::matching::l3::error::MatchingL3Error;
    to_py_err(
        MatchingL3Error::AssetNotFound {
            symbol: Symbol::from(symbol),
        }
        .into(),
    )
}

/// `BatchMode` 字符串解析(大小写不敏感)
fn parse_batch_mode(s: &str) -> PyResult<BatchMode> {
    match s.to_lowercase().as_str() {
        "continuous" => Ok(BatchMode::Continuous),
        "auction" => Ok(BatchMode::Auction),
        "dark_pool" | "darkpool" => Ok(BatchMode::DarkPool),
        other => Err(PyValueError::new_err(format!(
            "invalid batch mode: {other} (expected 'continuous' / 'auction' / 'dark_pool')"
        ))),
    }
}

/// `BatchMode` → 字符串(与 `parse_batch_mode` 对偶)
fn batch_mode_to_str(m: BatchMode) -> &'static str {
    match m {
        BatchMode::Continuous => "continuous",
        BatchMode::Auction => "auction",
        BatchMode::DarkPool => "dark_pool",
    }
}

/// `CrossPair` 来源适配:既支持 `CrossPair` `#[pyclass]` 实例,
/// 也支持 `dict {"leg1": ..., "leg2": ..., "ratio": ..., "max_quantity": ...}`
///
/// `py` 当前未被函数体使用(`extract` / `cast` 不需要显式 `Python` 句柄),
/// 保留它是为了与 `submit_dark_order` 等"接收 `Bound<'_, PyAny>`"的
/// 方法签名保持一致(`extract` 失败回退到 `cast` 时 `py` 不需要,
/// 但调用方调用本函数时仍可使用 `py` 做其他操作)。
fn cross_pair_from_any<'py>(_py: Python<'py>, obj: &Bound<'py, PyAny>) -> PyResult<RustCrossPair> {
    // 优先尝试 `CrossPair` `#[pyclass]` 实例
    if let Ok(pair) = obj.extract::<PyCrossPair>() {
        return Ok(pair.to_rust());
    }
    // 退回 `dict` 路径
    let dict = obj
        .cast::<PyDict>()
        .map_err(|_e| PyValueError::new_err("cross_pair must be a CrossPair or dict"))?;
    let leg1: String = require_dict_field(dict, "leg1")?;
    let leg2: String = require_dict_field(dict, "leg2")?;
    let ratio: f64 = require_dict_field(dict, "ratio")?;
    let max_quantity: f64 = require_dict_field(dict, "max_quantity")?;
    Ok(RustCrossPair::new(
        Symbol::from(leg1),
        Symbol::from(leg2),
        ratio,
        Quantity::from_f64(max_quantity),
    ))
}

/// 从 dict 提取必填字段(与 `super::types::require_field` 镜像,但不导出)
fn require_dict_field<'py, T>(dict: &Bound<'py, PyDict>, field: &str) -> PyResult<T>
where
    T: pyo3::conversion::FromPyObjectOwned<'py>,
{
    let v = dict
        .get_item(field)?
        .ok_or_else(|| PyKeyError::new_err(format!("missing '{field}'")))?;
    v.extract::<T>()
        .map_err(|_e| PyValueError::new_err(format!("field '{field}' has wrong type or value")))
}

/// `Vec<MatchFill>` → Python `list[dict]`
fn fills_to_pylist<'py>(py: Python<'py>, fills: &[MatchFill]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for fill in fills {
        list.append(match_fill_to_dict(py, fill)?)?;
    }
    Ok(list)
}

/// `Vec<PriceLevel>` → Python `list[dict]`
///
/// 用于 `snapshot.engines[].bid_depth / ask_depth`(L3 `PriceLevel`,
/// 由 `OrderBookLevel` 转换而来,字段一致但语义不同:后者侧重"快照"形态)。
fn price_levels_to_pylist<'py>(
    py: Python<'py>,
    levels: &[RustPriceLevel],
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

/// `Vec<OrderBookLevel>` → Python `list[dict]`
///
/// 用于 `L2MatchingEngine::depth()` 返回的实时订单簿层
/// (与 `PriceLevel` 字段一致但类型不同,这里独立函数以避免借用
/// `OrderBookLevel` 借给 `price_levels_to_pylist` 时的类型不匹配)。
fn order_book_levels_to_pylist<'py>(
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

/// `L3Stats` → Python dict
fn l3_stats_to_dict<'py>(py: Python<'py>, s: &RustL3Stats) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("total_assets", s.total_assets)?;
    d.set_item("total_cross_fills", s.total_cross_fills)?;
    d.set_item("total_batch_fills", s.total_batch_fills)?;
    d.set_item("total_dark_fills", s.total_dark_fills)?;
    d.set_item("total_arbitrage_profit", s.total_arbitrage_profit)?;
    Ok(d)
}

/// `MatchingEngineSnapshot` → Python dict
fn snapshot_to_dict<'py>(
    py: Python<'py>,
    snap: &RustMatchingEngineSnapshot,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("batch_mode", batch_mode_to_str(snap.batch_mode))?;
    d.set_item("timestamp_ns", snap.timestamp_ns)?;
    // engines: dict[symbol, L2Snapshot dict]
    let engines_dict = PyDict::new(py);
    for (sym, l2) in &snap.engines {
        let l2_d = PyDict::new(py);
        l2_d.set_item("symbol", sym.to_string())?;
        l2_d.set_item("best_bid", l2.best_bid.map(|p| p.as_f64()))?;
        l2_d.set_item("best_ask", l2.best_ask.map(|p| p.as_f64()))?;
        l2_d.set_item("bid_depth", price_levels_to_pylist(py, &l2.bid_depth)?)?;
        l2_d.set_item("ask_depth", price_levels_to_pylist(py, &l2.ask_depth)?)?;
        l2_d.set_item("trade_count", l2.trade_count)?;
        engines_dict.set_item(sym.to_string(), l2_d)?;
    }
    d.set_item("engines", engines_dict)?;
    // cross_pairs: list[dict]
    let pairs = PyList::empty(py);
    for cp in &snap.cross_pairs {
        let cp_d = PyDict::new(py);
        cp_d.set_item("leg1", cp.leg1.to_string())?;
        cp_d.set_item("leg2", cp.leg2.to_string())?;
        cp_d.set_item("ratio", cp.ratio)?;
        cp_d.set_item("max_quantity", cp.max_quantity.as_f64())?;
        pairs.append(cp_d)?;
    }
    d.set_item("cross_pairs", pairs)?;
    Ok(d)
}

/// Python dict → `MatchingEngineSnapshot`
///
/// 注:这里**不**做完整价格级别恢复(与 Rust `restore` 一致),只重建
/// 资产注册、跨资产配置和批量模式。`engines` 字段被解析以拿到 symbol
/// 列表;`bid_depth` / `ask_depth` 字段在 Rust 端被忽略。
fn snapshot_from_dict<'py>(
    py: Python<'py>,
    snap: &Bound<'py, PyDict>,
) -> PyResult<RustMatchingEngineSnapshot> {
    // batch_mode
    let mode_str: String = require_dict_field(snap, "batch_mode")?;
    let batch_mode = parse_batch_mode(&mode_str)?;
    // engines: dict[symbol, _]
    let engines_dict: Bound<'py, PyDict> = require_dict_field(snap, "engines")?;
    let mut engines = std::collections::HashMap::new();
    for (key, value) in engines_dict.iter() {
        let symbol: String = key.extract()?;
        let l2_d = value
            .cast::<PyDict>()
            .map_err(|_e| PyValueError::new_err("engines values must be dicts"))?;
        let best_bid: Option<f64> = require_dict_field(l2_d, "best_bid")?;
        let best_ask: Option<f64> = require_dict_field(l2_d, "best_ask")?;
        let trade_count: u64 = require_dict_field(l2_d, "trade_count")?;
        // bid_depth / ask_depth 解析为 list[dict],但内容会被忽略(只读 symbol / 价格点)
        // —— 为不破坏字段契约仍解析,失败时容忍。
        let bid_depth = parse_price_levels(py, l2_d, "bid_depth").unwrap_or_default();
        let ask_depth = parse_price_levels(py, l2_d, "ask_depth").unwrap_or_default();
        engines.insert(
            Symbol::from(symbol),
            crate::matching::l3::types::L2Snapshot {
                symbol: Symbol::from(extract_symbol_from_l2(l2_d)),
                best_bid: best_bid.map(Price::from_f64),
                best_ask: best_ask.map(Price::from_f64),
                bid_depth,
                ask_depth,
                trade_count,
            },
        );
    }
    // cross_pairs
    let pairs_list: Bound<'py, PyList> = require_dict_field(snap, "cross_pairs")?;
    let mut cross_pairs = Vec::with_capacity(pairs_list.len());
    for item in pairs_list.iter() {
        let cp = cross_pair_from_any(py, &item)?;
        cross_pairs.push(cp);
    }
    // timestamp_ns(可选)
    let timestamp_ns: u64 = snap
        .get_item("timestamp_ns")?
        .and_then(|v| v.extract::<u64>().ok())
        .unwrap_or(0);
    Ok(RustMatchingEngineSnapshot {
        engines,
        cross_pairs,
        batch_mode,
        timestamp_ns,
    })
}

/// 解析 `bid_depth` / `ask_depth` 字段(失败时返回空 Vec)
fn parse_price_levels<'py>(
    _py: Python<'py>,
    dict: &Bound<'py, PyDict>,
    field: &str,
) -> Option<Vec<RustPriceLevel>> {
    let v = dict.get_item(field).ok()??;
    let list = v.cast::<PyList>().ok()?;
    let mut out = Vec::with_capacity(list.len());
    for item in list.iter() {
        let d = item.cast::<PyDict>().ok()?;
        let price: f64 = d.get_item("price").ok()??.extract().ok()?;
        let quantity: f64 = d.get_item("quantity").ok()??.extract().ok()?;
        let order_count: usize = d.get_item("order_count").ok()??.extract().ok()?;
        out.push(RustPriceLevel {
            price: Price::from_f64(price),
            quantity: Quantity::from_f64(quantity),
            order_count,
        });
    }
    Some(out)
}

/// 从 L2 snapshot dict 拿 `symbol` 字段(用于构造 Rust `L2Snapshot.symbol`,
/// 字段缺失时返回空字符串)
fn extract_symbol_from_l2<'py>(dict: &Bound<'py, PyDict>) -> String {
    dict.get_item("symbol")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_default()
}

/// 在 `_native.backtest` 子模块下注册 L3 配套类。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyMultiAssetMatchingEngine>()?;
    parent.add_class::<PyDarkOrder>()?;
    parent.add_class::<PyCrossPair>()?;
    parent.add_class::<PyAuctionResult>()?;
    parent.add_class::<PyArbitrageOpportunity>()?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyDict;

    fn make_limit_dict<'py>(
        py: Python<'py>,
        id: u64,
        symbol: &str,
        side: &str,
        price: f64,
        qty: f64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("id", id)?;
        d.set_item("symbol", symbol)?;
        d.set_item("side", side)?;
        d.set_item("type", "limit")?;
        d.set_item("price", price)?;
        d.set_item("quantity", qty)?;
        d.set_item("tif", "GTC")?;
        Ok(d)
    }

    /// `__repr__` 含资产 / 跨资产 / 模式
    #[test]
    fn repr_contains_l3_fields() {
        let mut m = PyMultiAssetMatchingEngine::new();
        m.register_asset("BTC-USDT");
        m.register_asset("ETH-USDT");
        m.set_batch_mode("auction").unwrap();
        let s = m.__repr__();
        assert!(s.contains("MultiAssetMatchingEngine"));
        assert!(s.contains("assets=2"));
        assert!(s.contains("batch_mode=auction"));
    }

    /// 空 L3:资产 / 跨资产 / 模式 默认值
    #[test]
    fn empty_l3_defaults() {
        let m = PyMultiAssetMatchingEngine::new();
        assert_eq!(m.asset_count(), 0);
        assert_eq!(m.cross_pair_count(), 0);
        assert_eq!(m.batch_mode(), "continuous");
    }

    /// `register_asset` 幂等
    #[test]
    fn register_asset_idempotent() {
        let mut m = PyMultiAssetMatchingEngine::new();
        m.register_asset("BTC-USDT");
        m.register_asset("BTC-USDT");
        assert_eq!(m.asset_count(), 1);
    }

    /// `set_batch_mode` / `batch_mode` getter 圆环
    #[test]
    fn set_batch_mode_roundtrip() {
        let mut m = PyMultiAssetMatchingEngine::new();
        m.set_batch_mode("auction").unwrap();
        assert_eq!(m.batch_mode(), "auction");
        m.set_batch_mode("dark_pool").unwrap();
        assert_eq!(m.batch_mode(), "dark_pool");
        m.set_batch_mode("continuous").unwrap();
        assert_eq!(m.batch_mode(), "continuous");
    }

    /// `set_batch_mode` 非法值 → `PyValueError`
    #[test]
    fn set_batch_mode_invalid_raises() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            let err = m.set_batch_mode("nope").unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    /// 多资产路由:连续模式下 submit 后另一资产订单簿不受影响
    #[test]
    fn multi_asset_routing_isolates_books() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            m.register_asset("BTC-USDT");
            m.register_asset("ETH-USDT");

            // BTC:卖 @ 50000
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 50000.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            // ETH:另一资产,无最优买价
            assert!(m.best_bid("ETH-USDT").unwrap().is_none());
            // BTC:最优卖价
            assert_eq!(m.best_ask("BTC-USDT").unwrap(), Some(50000.0));
        });
    }

    /// per-symbol 资产未注册时抛 `BacktestError(code="MatchingL3")`
    #[test]
    fn best_bid_unknown_asset_raises_backtest_error() {
        let m = PyMultiAssetMatchingEngine::new();
        let err = m.best_bid("UNKNOWN").unwrap_err();
        let s = err.to_string();
        assert!(
            s.contains("[MatchingL3]"),
            "expected [MatchingL3], got: {s}"
        );
    }

    /// 连续模式下 submit 撮合正确
    #[test]
    fn submit_continuous_mode_yields_fill() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            m.register_asset("BTC-USDT");
            let sell = make_limit_dict(py, 1, "BTC-USDT", "sell", 100.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            let buy = make_limit_dict(py, 2, "BTC-USDT", "buy", 100.0, 1.0).unwrap();
            let fills = m.submit(py, &buy).unwrap();
            assert_eq!(fills.len(), 1);
        });
    }

    /// 拍卖模式下 submit 暂存,run_auction 清算
    #[test]
    fn auction_mode_defers_orders() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            m.register_asset("ETH-USDT");
            m.set_batch_mode("auction").unwrap();

            let buy = make_limit_dict(py, 1, "ETH-USDT", "buy", 3000.0, 5.0).unwrap();
            let fills = m.submit(py, &buy).unwrap();
            assert_eq!(fills.len(), 0, "Auction 模式应暂存订单");

            let sell = make_limit_dict(py, 2, "ETH-USDT", "sell", 3002.0, 5.0).unwrap();
            m.submit(py, &sell).unwrap();

            let result = m.run_auction("ETH-USDT").unwrap();
            assert!(result.has_trades());
            assert!(result.clearing_volume() > 0.0);
        });
    }

    /// 暗池订单:无对手方 → 暂存
    #[test]
    fn submit_dark_order_no_match_stores() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            m.register_asset("BTC-USDT");
            let dark = PyDarkOrder::new(
                1,
                "BTC-USDT",
                "buy",
                "limit",
                5.0,
                "GTC",
                2.0, // visible
                5.0, // hidden
                Some(50000.0),
            )
            .unwrap();
            let fills = m.submit_dark_order(py, &dark).unwrap();
            assert_eq!(fills.len(), 0);
            let stats = m.stats(py).unwrap();
            assert_eq!(
                stats
                    .get_item("total_dark_fills")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                0
            );
        });
    }

    /// 暗池订单:有对手方 → 成交
    #[test]
    fn submit_dark_order_matches_existing() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            m.register_asset("BTC-USDT");

            let sell = PyDarkOrder::new(
                1,
                "BTC-USDT",
                "sell",
                "limit",
                3.0,
                "GTC",
                1.0,
                3.0,
                Some(50000.0),
            )
            .unwrap();
            m.submit_dark_order(py, &sell).unwrap();

            let buy = PyDarkOrder::new(
                2,
                "BTC-USDT",
                "buy",
                "limit",
                3.0,
                "GTC",
                1.0,
                3.0,
                Some(50000.0),
            )
            .unwrap();
            let fills = m.submit_dark_order(py, &buy).unwrap();
            assert_eq!(fills.len(), 1);
            let stats = m.stats(py).unwrap();
            assert_eq!(
                stats
                    .get_item("total_dark_fills")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                1
            );
        });
    }

    /// `DarkOrder` visible > hidden 校验在 to_rust 时返回 `InvalidDarkOrderQuantity`
    #[test]
    fn dark_order_invalid_visible_exceeds_hidden() {
        let dark = PyDarkOrder::new(
            1,
            "BTC-USDT",
            "buy",
            "limit",
            5.0,
            "GTC",
            10.0, // visible > hidden
            5.0,
            Some(100.0),
        )
        .unwrap();
        let err = dark.to_rust().unwrap_err();
        assert!(matches!(
            err,
            crate::matching::l3::error::MatchingL3Error::InvalidDarkOrderQuantity { .. }
        ));
    }

    /// `register_cross_pair` dict 路径
    #[test]
    fn register_cross_pair_from_dict() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            let d = PyDict::new(py);
            d.set_item("leg1", "BTC-USDT").unwrap();
            d.set_item("leg2", "ETH-USDT").unwrap();
            d.set_item("ratio", 16.0_f64).unwrap();
            d.set_item("max_quantity", 1.0_f64).unwrap();
            m.register_cross_pair(py, &d).unwrap();
            assert_eq!(m.asset_count(), 2);
            assert_eq!(m.cross_pair_count(), 1);
        });
    }

    /// `register_cross_pair` 非法 ratio → `BacktestError`
    #[test]
    fn register_cross_pair_invalid_ratio_raises() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            let cp = PyCrossPair::new("BTC-USDT", "ETH-USDT", 0.0, 1.0);
            let err = m
                .register_cross_pair(py, &cp.into_pyobject(py).unwrap())
                .unwrap_err();
            let s = err.to_string();
            assert!(
                s.contains("[MatchingL3]"),
                "expected [MatchingL3], got: {s}"
            );
        });
    }

    /// `register_cross_pair` leg1 == leg2 → `BacktestError`
    #[test]
    fn register_cross_pair_same_leg_raises() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            let cp = PyCrossPair::new("BTC-USDT", "BTC-USDT", 1.0, 1.0);
            let err = m
                .register_cross_pair(py, &cp.into_pyobject(py).unwrap())
                .unwrap_err();
            assert!(err.to_string().contains("[MatchingL3]"));
        });
    }

    /// `detect_arbitrage` 在买卖价都存在时返回非空 `implied_ratio`
    #[test]
    fn detect_arbitrage_with_both_sides() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            let cp = PyCrossPair::new("BTC-USDT", "ETH-USDT", 16.0, 1.0);
            m.register_cross_pair(py, &cp.into_pyobject(py).unwrap())
                .unwrap();

            // BTC 买/卖
            let b1 = make_limit_dict(py, 1, "BTC-USDT", "buy", 50000.0, 1.0).unwrap();
            m.submit(py, &b1).unwrap();
            let b2 = make_limit_dict(py, 2, "BTC-USDT", "sell", 50100.0, 1.0).unwrap();
            m.submit(py, &b2).unwrap();
            // ETH 买/卖
            let e1 = make_limit_dict(py, 3, "ETH-USDT", "buy", 3000.0, 1.0).unwrap();
            m.submit(py, &e1).unwrap();
            let e2 = make_limit_dict(py, 4, "ETH-USDT", "sell", 3020.0, 1.0).unwrap();
            m.submit(py, &e2).unwrap();

            let ops = m.detect_arbitrage(py).unwrap();
            assert_eq!(ops.len(), 1);
            // implied_ratio 不为 None
            let op = ops.get_item(0).unwrap();
            let implied: Option<f64> = op.getattr("implied_ratio").unwrap().extract().unwrap();
            assert!(implied.is_some());
            let dev: f64 = op.getattr("deviation").unwrap().extract().unwrap();
            assert!(dev > 0.0);
        });
    }

    /// `snapshot` → `restore` 圆环
    #[test]
    fn snapshot_restore_roundtrip() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            m.register_asset("BTC-USDT");
            m.register_asset("ETH-USDT");
            m.set_batch_mode("auction").unwrap();

            let snap = m.snapshot(py).unwrap();
            // 验证关键字段存在
            assert_eq!(
                snap.get_item("batch_mode")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "auction"
            );
            let engines = snap.get_item("engines").unwrap().unwrap();
            assert_eq!(engines.len().unwrap(), 2);

            // 还原到新引擎
            let mut m2 = PyMultiAssetMatchingEngine::new();
            m2.restore(py, &snap).unwrap();
            assert_eq!(m2.asset_count(), 2);
            assert_eq!(m2.batch_mode(), "auction");
        });
    }

    /// `register` 函数签名稳定(编译期断言)
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
