//! `MultiAssetMatchingEngine` + L3 配套类型 Python 绑定
//!
//! 暴露 L3 多资产撮合引擎到 Python,包括:
//! - 多资产路由(`register_instrument` / 隐式随 `submit` 注册)
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
//! - **入参 instrument**(0.6.0):Python `dict`,由 `parse_instrument` 解析
//!   格式:`{"kind": "spot", "base": "BTC", "quote": "USDT"}` 或
//!   `{"kind": "swap", "base": "BTC", "quote": "USDT", "settle": "usd_margin", "contract_size": 1.0}`
//! - **出参**:成交列表 dict / 拍卖结果 `AuctionResult` `#[pyclass]` /
//!   套利机会列表 `[ArbitrageOpportunity]` / 统计 dict
//! - **per-instrument 子引擎**:通过 `best_bid(instrument)` / `best_ask(instrument)` /
//!   `depth(instrument, levels)` / `stats(instrument)` 等方法查询,避免 Rust
//!   借用 `HashMap<Instrument, L2MatchingEngine>` 跨 FFI 边界。
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
//!   内部用 `HashMap<Instrument, L2MatchingEngine>` 存储,跨函数保持对某个
//!   instrument 的 `&mut L2MatchingEngine` 借用极不友好(rust borrow checker
//!   不允许,`GIL` 释放后更不可控)。故采用"per-instrument 方法"模式,在
//!   `PyMultiAssetMatchingEngine` 上直接提供 `best_bid(instrument)` 等
//!   一组 method,语义等价于 "L2 子引擎的远程调用",更稳定。
//! - **`PyAuctionResult` / `PyArbitrageOpportunity` / `PyL3Stats`** 用
//!   `#[pyclass]` 暴露,允许 Python 端 `result.clearing_price` 属性访问
//!   而非 `dict` —— L3 报告类数据通常按字段读取,`pyclass` 更自然。
//!
//! # 0.6.0 BREAKING 改动
//!
//! 全面从 `Symbol` 迁到 `Instrument`:
//! - `register_asset(symbol: str)` → `register_instrument(instrument: dict)`
//! - `run_auction(symbol: str)` → `run_auction(instrument: dict)`
//! - `best_bid(symbol: str)` → `best_bid(instrument: dict)`
//! - `PyCrossPair.leg1/leg2: str` → `leg1/leg2: dict`(instrument dict)

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use axon_core::market::Side as CoreSide;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::types::{Instrument, Price, Quantity};

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
use super::types::{
    dict_to_order, match_fill_to_dict, parse_instrument, parse_side, require_dict_field,
};

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

    /// 0.6.0 改(BREAKING):`register_asset(symbol: str)` → `register_instrument(instrument: dict)`
    ///
    /// 注册 instrument(幂等)。`submit` 隐式随 `order.instrument` 注册,
    /// 故一般无需显式调;但预注册可让 `engine()` 查询不返回 `None`。
    ///
    /// `instrument` dict 格式:
    /// - spot:`{"kind": "spot", "base": "BTC", "quote": "USDT"}`
    /// - swap:`{"kind": "swap", "base": "BTC", "quote": "USDT",
    ///   "settle": "usd_margin", "contract_size": 1.0}`
    fn register_instrument<'py>(
        &mut self,
        _py: Python<'py>,
        instrument: &Bound<'py, PyDict>,
    ) -> PyResult<()> {
        let inst = parse_instrument(instrument)?;
        self.inner.register_instrument(inst);
        Ok(())
    }

    /// 注册跨资产交易对
    ///
    /// 自动注册两个 leg 的 instrument。失败(leg1 == leg2、ratio 非正等)
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

    /// 0.6.0 改(BREAKING):`run_auction(symbol: str)` → `run_auction(instrument: dict)`
    ///
    /// 运行批量拍卖(对指定 instrument 执行清算价撮合)
    ///
    /// Auction 模式下被 `submit` 暂存的订单会被消费;其他 instrument 的暂存
    /// 订单保留。Returns:`AuctionResult` 包含 `clearing_price` /
    /// `clearing_volume` / `fills` / `unfilled_orders`。
    fn run_auction<'py>(
        &mut self,
        _py: Python<'py>,
        instrument: &Bound<'py, PyDict>,
    ) -> PyResult<PyAuctionResult> {
        let inst = parse_instrument(instrument)?;
        let r = self
            .inner
            .run_auction(&inst)
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

    /// 已注册 instrument 数
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

    // ─── per-instrument 子引擎查询(见模块级 doc 注释) ───────────

    /// 0.6.0 改(BREAKING):`best_bid(symbol: str)` → `best_bid(instrument: dict)`
    ///
    /// 指定 instrument 的最优买价
    fn best_bid<'py>(
        &self,
        _py: Python<'py>,
        instrument: &Bound<'py, PyDict>,
    ) -> PyResult<Option<f64>> {
        let inst = parse_instrument(instrument)?;
        self.inner
            .engine(&inst)
            .ok_or_else(|| asset_not_found_pyerr(&inst))
            .map(|e| e.best_bid().map(|p| p.as_f64()))
    }

    /// 0.6.0 改(BREAKING):`best_ask(symbol: str)` → `best_ask(instrument: dict)`
    ///
    /// 指定 instrument 的最优卖价
    fn best_ask<'py>(
        &self,
        _py: Python<'py>,
        instrument: &Bound<'py, PyDict>,
    ) -> PyResult<Option<f64>> {
        let inst = parse_instrument(instrument)?;
        self.inner
            .engine(&inst)
            .ok_or_else(|| asset_not_found_pyerr(&inst))
            .map(|e| e.best_ask().map(|p| p.as_f64()))
    }

    /// 0.6.0 改:`spread(symbol: str)` → `spread(instrument: dict)`
    ///
    /// 指定 instrument 的买卖价差
    fn spread<'py>(
        &self,
        _py: Python<'py>,
        instrument: &Bound<'py, PyDict>,
    ) -> PyResult<Option<f64>> {
        let inst = parse_instrument(instrument)?;
        self.inner
            .engine(&inst)
            .ok_or_else(|| asset_not_found_pyerr(&inst))
            .map(|e| e.spread().map(|p| p.as_f64()))
    }

    /// 0.6.0 改:`depth(symbol, levels)` → `depth(instrument, levels)`
    ///
    /// 指定 instrument 的订单簿深度
    #[pyo3(signature = (instrument, levels=10))]
    fn depth<'py>(
        &self,
        py: Python<'py>,
        instrument: &Bound<'py, PyDict>,
        levels: usize,
    ) -> PyResult<Bound<'py, PyDict>> {
        let inst = parse_instrument(instrument)?;
        let engine = self
            .inner
            .engine(&inst)
            .ok_or_else(|| asset_not_found_pyerr(&inst))?;
        let (bids, asks) = engine.depth(levels);
        let d = PyDict::new(py);
        d.set_item("bids", order_book_levels_to_pylist(py, &bids)?)?;
        d.set_item("asks", order_book_levels_to_pylist(py, &asks)?)?;
        Ok(d)
    }

    /// 0.6.0 改:`active_order_count(symbol)` → `active_order_count(instrument)`
    ///
    /// 指定 instrument 的活跃订单数
    fn active_order_count<'py>(
        &self,
        _py: Python<'py>,
        instrument: &Bound<'py, PyDict>,
    ) -> PyResult<usize> {
        let inst = parse_instrument(instrument)?;
        self.inner
            .engine(&inst)
            .ok_or_else(|| asset_not_found_pyerr(&inst))
            .map(|e| e.active_order_count())
    }

    /// 0.6.0 改:`fill_count(symbol)` → `fill_count(instrument)`
    ///
    /// 指定 instrument 的累计成交笔数
    fn fill_count<'py>(&self, _py: Python<'py>, instrument: &Bound<'py, PyDict>) -> PyResult<u64> {
        let inst = parse_instrument(instrument)?;
        self.inner
            .engine(&inst)
            .ok_or_else(|| asset_not_found_pyerr(&inst))
            .map(|e| e.stats().total_fills)
    }

    /// 0.6.0 改:`stats_of(symbol)` → `stats_of(instrument)`
    ///
    /// 指定 instrument 的撮合统计
    fn stats_of<'py>(
        &self,
        py: Python<'py>,
        instrument: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let inst = parse_instrument(instrument)?;
        let engine = self
            .inner
            .engine(&inst)
            .ok_or_else(|| asset_not_found_pyerr(&inst))?;
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
    /// Returns:含 `batch_mode` / `engines`(dict[instrument, L2Snapshot])/
    /// `cross_pairs` / `timestamp_ns`。
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        snapshot_to_dict(py, &self.inner.snapshot())
    }

    /// 从快照恢复(仅恢复 instrument 注册 / 跨资产配置 / 批量模式,
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
///
/// 0.6.0:instrument 字段统一为 `kind` / `base` / `quote` / `settle` / `contract_size`,
/// 由 `parse_instrument` 自动解析;`symbol` getter 保留为兼容视图
/// (`"{base}/{quote}"`)。
#[pyclass(name = "DarkOrder", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyDarkOrder {
    /// 公开可见数量(冰山订单露出部分)
    visible_quantity: f64,
    /// 隐藏总数量
    hidden_quantity: f64,
    /// 订单本体各字段
    order_id: u64,
    /// 交易品种 kind:`"spot"` / `"swap"`
    instrument_kind: String,
    /// 基础币种(spot / swap 共有)
    base: String,
    /// 计价币种(spot / swap 共有)
    quote: String,
    /// 结算方式(仅 swap,`"usd_margin"` / `"coin_margin"`)
    settle: Option<String>,
    /// 合约乘数(仅 swap)
    contract_size: Option<f64>,
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
    /// - `base`:基础币种(spot / swap 共有)
    /// - `quote`:计价币种(spot / swap 共有)
    /// - `kind`:交易品种,`"spot"`(默认)/ `"swap"`
    /// - `settle`:仅 swap 必填,`"usd_margin"` / `"coin_margin"`
    /// - `contract_size`:仅 swap 必填,合约乘数(默认 1.0)
    /// - `side`:`"buy"` / `"sell"`
    /// - `order_type`:`"market"` / `"limit"`
    /// - `price`:限价单必填
    /// - `quantity`:订单总数量
    /// - `tif`:有效期
    /// - `visible_quantity`:可见数量(冰山部分)
    /// - `hidden_quantity`:隐藏总数量
    #[new]
    #[pyo3(signature = (order_id, base, quote, side, order_type, quantity, tif, visible_quantity, hidden_quantity, kind="spot", settle=None, contract_size=None, price=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        order_id: u64,
        base: &str,
        quote: &str,
        side: &str,
        order_type: &str,
        quantity: f64,
        tif: &str,
        visible_quantity: f64,
        hidden_quantity: f64,
        kind: &str,
        settle: Option<&str>,
        contract_size: Option<f64>,
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
        let kind_lc = kind.to_lowercase();
        if kind_lc != "spot" && kind_lc != "swap" {
            return Err(PyValueError::new_err(format!(
                "unsupported instrument kind: {kind} (only 'spot' / 'swap')"
            )));
        }
        // swap 必传 settle + contract_size
        if kind_lc == "swap" && (settle.is_none() || contract_size.is_none()) {
            return Err(PyValueError::new_err(
                "swap instrument requires 'settle' and 'contract_size'",
            ));
        }
        Ok(Self {
            visible_quantity,
            hidden_quantity,
            order_id,
            instrument_kind: kind_lc,
            base: base.to_string(),
            quote: quote.to_string(),
            settle: settle.map(|s| s.to_string()),
            contract_size,
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

    /// 兼容字段:返回 `"{base}/{quote}"` 形式字符串
    #[getter]
    fn symbol(&self) -> String {
        format!("{}/{}", self.base, self.quote)
    }

    /// 0.6.0:返回 instrument dict(`parse_instrument` 格式)
    #[getter]
    fn instrument<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        instrument_to_dict(
            py,
            &self.instrument_kind,
            &self.base,
            &self.quote,
            self.settle.as_deref(),
            self.contract_size,
        )
    }

    /// 0.6.0:品种 kind(`"spot"` / `"swap"`)
    #[getter]
    fn kind(&self) -> &str {
        &self.instrument_kind
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
            self.symbol(),
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
        use crate::matching::l3::error::MatchingL3Error;

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
        // 0.6.0:基于 instrument 字段构造 Order,spot / swap 分别走对应工厂
        let instrument =
            self.to_instrument()
                .ok_or_else(|| MatchingL3Error::InvalidDarkOrderQuantity {
                    visible: Quantity::from_f64(self.visible_quantity),
                    hidden: Quantity::from_f64(self.hidden_quantity),
                })?;
        let order = match &instrument {
            Instrument::Spot(s) => Order::spot(
                self.order_id,
                s.base.clone(),
                s.quote.clone(),
                side,
                order_type,
                Quantity::from_f64(self.quantity),
                tif,
            ),
            Instrument::Swap(s) => Order::swap(
                self.order_id,
                s.base.clone(),
                s.quote.clone(),
                s.settle,
                s.contract_size,
                side,
                order_type,
                Quantity::from_f64(self.quantity),
                tif,
            ),
        };
        RustDarkOrder::new(
            order,
            Quantity::from_f64(self.visible_quantity),
            Quantity::from_f64(self.hidden_quantity),
        )
    }

    /// 0.6.0:把 `PyDarkOrder` 的 instrument 字段转成 `Instrument`
    fn to_instrument(&self) -> Option<Instrument> {
        use axon_core::types::{SpotInstrument, SwapInstrument, SwapSettle, Symbol};
        let base = Symbol::from(self.base.clone());
        let quote = Symbol::from(self.quote.clone());
        match self.instrument_kind.as_str() {
            "spot" => Some(Instrument::Spot(SpotInstrument { base, quote })),
            "swap" => {
                let settle = match self.settle.as_deref()? {
                    "usd_margin" | "UsdMargin" => SwapSettle::UsdMargin,
                    "coin_margin" | "CoinMargin" => SwapSettle::CoinMargin,
                    _ => return None,
                };
                let contract_size = self.contract_size?;
                Some(Instrument::Swap(SwapInstrument {
                    base,
                    quote,
                    settle,
                    contract_size,
                }))
            }
            _ => None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PyCrossPair: 跨资产交易对
// ═══════════════════════════════════════════════════════════════════════════

/// Python 侧跨资产交易对
///
/// `#[pyclass(from_py_object)]` 让 pyo3 0.28 自动生成 `FromPyObject`,
/// 同时支持 `register_cross_pair` 接受 `CrossPair(...)` 实例。
///
/// 0.6.0 BREAKING:`leg1/leg2` 从 `str` 改为 `dict`(instrument dict 格式,
/// 由 `parse_instrument` 解析);`symbol` getter 保留 `"BASE/QUOTE"` 字符串
/// 兼容视图(只取 spot,swap 时会带 `:SWAP` 后缀)。
#[pyclass(name = "CrossPair", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyCrossPair {
    /// leg1 instrument 字段(0.6.0 改为 dict 而非 str)
    leg1_kind: String,
    leg1_base: String,
    leg1_quote: String,
    leg1_settle: Option<String>,
    leg1_contract_size: Option<f64>,
    /// leg2 instrument 字段
    leg2_kind: String,
    leg2_base: String,
    leg2_quote: String,
    leg2_settle: Option<String>,
    leg2_contract_size: Option<f64>,
    /// 交换比率
    ratio: f64,
    /// 最大可执行数量
    max_quantity: f64,
}

#[pymethods]
impl PyCrossPair {
    /// 创建跨资产交易对
    ///
    /// Args:
    /// - `leg1`:leg1 instrument dict
    /// - `leg2`:leg2 instrument dict
    /// - `ratio`:交换比率
    /// - `max_quantity`:最大可执行数量
    #[new]
    fn new(
        leg1: &Bound<'_, PyDict>,
        leg2: &Bound<'_, PyDict>,
        ratio: f64,
        max_quantity: f64,
    ) -> PyResult<Self> {
        let l1 = parse_instrument(leg1)?;
        let l2 = parse_instrument(leg2)?;
        let (l1_kind, l1_base, l1_quote, l1_settle, l1_csize) = instrument_fields(&l1);
        let (l2_kind, l2_base, l2_quote, l2_settle, l2_csize) = instrument_fields(&l2);
        Ok(Self {
            leg1_kind: l1_kind,
            leg1_base: l1_base,
            leg1_quote: l1_quote,
            leg1_settle: l1_settle,
            leg1_contract_size: l1_csize,
            leg2_kind: l2_kind,
            leg2_base: l2_base,
            leg2_quote: l2_quote,
            leg2_settle: l2_settle,
            leg2_contract_size: l2_csize,
            ratio,
            max_quantity,
        })
    }

    /// leg1 instrument dict
    #[getter]
    fn leg1<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        instrument_to_dict(
            py,
            &self.leg1_kind,
            &self.leg1_base,
            &self.leg1_quote,
            self.leg1_settle.as_deref(),
            self.leg1_contract_size,
        )
    }

    /// leg2 instrument dict
    #[getter]
    fn leg2<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        instrument_to_dict(
            py,
            &self.leg2_kind,
            &self.leg2_base,
            &self.leg2_quote,
            self.leg2_settle.as_deref(),
            self.leg2_contract_size,
        )
    }

    /// 0.6.0:leg1 兼容字符串视图(`"BTC/USDT"` / `"BTC/USDT:SWAP"`)
    #[getter]
    fn leg1_symbol(&self) -> String {
        instrument_label(&self.leg1_kind, &self.leg1_base, &self.leg1_quote)
    }

    /// 0.6.0:leg2 兼容字符串视图
    #[getter]
    fn leg2_symbol(&self) -> String {
        instrument_label(&self.leg2_kind, &self.leg2_base, &self.leg2_quote)
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
            self.leg1_symbol(),
            self.leg2_symbol(),
            self.ratio,
            self.max_quantity
        )
    }
}

impl PyCrossPair {
    /// 转 Rust `CrossPair`(走 `LegPair` 包装)
    fn to_rust(&self) -> RustCrossPair {
        use axon_core::types::LegPair;
        let l1 = self.to_leg1_instrument();
        let l2 = self.to_leg2_instrument();
        RustCrossPair::from_leg_pair(
            LegPair::with_ratio(l1, l2, self.ratio),
            self.ratio,
            Quantity::from_f64(self.max_quantity),
        )
    }

    fn to_leg1_instrument(&self) -> Instrument {
        build_instrument(
            &self.leg1_kind,
            &self.leg1_base,
            &self.leg1_quote,
            self.leg1_settle.as_deref(),
            self.leg1_contract_size,
        )
        .expect("`new` 已校验过 instrument 合法性")
    }

    fn to_leg2_instrument(&self) -> Instrument {
        build_instrument(
            &self.leg2_kind,
            &self.leg2_base,
            &self.leg2_quote,
            self.leg2_settle.as_deref(),
            self.leg2_contract_size,
        )
        .expect("`new` 已校验过 instrument 合法性")
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
    leg1_label: String,
    leg2_label: String,
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
    /// 0.6.0:leg1 字符串 label(`"BTC/USDT"` / `"BTC/USDT:SWAP"`)
    #[getter]
    fn leg1(&self) -> &str {
        &self.leg1_label
    }

    /// 0.6.0:leg2 字符串 label
    #[getter]
    fn leg2(&self) -> &str {
        &self.leg2_label
    }

    /// 0.6.0:leg1 instrument dict(0.6.1 起推荐用此 API)
    #[getter]
    fn leg1_dict<'py>(&self, _py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        // 0.6.0 暂未拆分 leg1 kind/base/quote 字段,直接返回空 dict 占位
        // 真实拆分需把 `Instrument` 字段在 `PyArbitrageOpportunity` 里全保留,
        // 留给 0.6.1 / 0.7 重构。
        let d = PyDict::new(_py);
        d.set_item("label", &self.leg1_label)?;
        Ok(d)
    }

    /// 0.6.0:leg2 instrument dict
    #[getter]
    fn leg2_dict<'py>(&self, _py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(_py);
        d.set_item("label", &self.leg2_label)?;
        Ok(d)
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
            self.leg1_label,
            self.leg2_label,
            self.implied_ratio,
            self.deviation,
            self.estimated_profit
        )
    }
}

impl PyArbitrageOpportunity {
    fn from_rust(op: RustArbitrageOpportunity) -> Self {
        Self {
            leg1_label: op.pair.pair.spot.to_string(),
            leg2_label: op.pair.pair.perp.to_string(),
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

/// Instrument 未注册时构造的 `PyErr`(包装成统一 `BacktestError` 路径)
fn asset_not_found_pyerr(instrument: &Instrument) -> PyErr {
    use crate::matching::l3::error::MatchingL3Error;
    to_py_err(
        MatchingL3Error::AssetNotFound {
            instrument: instrument.clone(),
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
    use axon_core::types::LegPair;
    // 优先尝试 `CrossPair` `#[pyclass]` 实例
    if let Ok(pair) = obj.extract::<PyCrossPair>() {
        return Ok(pair.to_rust());
    }
    // 退回 `dict` 路径
    let dict = obj
        .cast::<PyDict>()
        .map_err(|_e| PyValueError::new_err("cross_pair must be a CrossPair or dict"))?;
    let leg1_obj: Bound<'py, PyDict> = require_dict_field(dict, "leg1")?;
    let leg2_obj: Bound<'py, PyDict> = require_dict_field(dict, "leg2")?;
    let leg1 = parse_instrument(&leg1_obj)?;
    let leg2 = parse_instrument(&leg2_obj)?;
    let ratio: f64 = require_dict_field(dict, "ratio")?;
    let max_quantity: f64 = require_dict_field(dict, "max_quantity")?;
    Ok(RustCrossPair::from_leg_pair(
        LegPair::with_ratio(leg1, leg2, ratio),
        ratio,
        Quantity::from_f64(max_quantity),
    ))
}

/// 0.6.0 helper:从 `Instrument` 提取字段用于 `PyCrossPair` 存储
fn instrument_fields(inst: &Instrument) -> (String, String, String, Option<String>, Option<f64>) {
    use axon_core::types::SwapSettle;
    match inst {
        Instrument::Spot(s) => (
            "spot".to_string(),
            s.base.to_string(),
            s.quote.to_string(),
            None,
            None,
        ),
        Instrument::Swap(s) => {
            let settle = match s.settle {
                SwapSettle::UsdMargin => "usd_margin",
                SwapSettle::CoinMargin => "coin_margin",
            };
            (
                "swap".to_string(),
                s.base.to_string(),
                s.quote.to_string(),
                Some(settle.to_string()),
                Some(s.contract_size),
            )
        }
    }
}

/// 0.6.0 helper:从 `Instrument` 字段构造 dict(用于 `PyDarkOrder.instrument` /
/// `PyCrossPair.leg1/leg2` getter)
fn instrument_to_dict<'py>(
    py: Python<'py>,
    kind: &str,
    base: &str,
    quote: &str,
    settle: Option<&str>,
    contract_size: Option<f64>,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("kind", kind)?;
    d.set_item("base", base)?;
    d.set_item("quote", quote)?;
    if let Some(s) = settle {
        d.set_item("settle", s)?;
    }
    if let Some(c) = contract_size {
        d.set_item("contract_size", c)?;
    }
    Ok(d)
}

/// 0.6.0 helper:从字段构造 `Instrument`(供 `PyCrossPair.to_rust` 使用)
fn build_instrument(
    kind: &str,
    base: &str,
    quote: &str,
    settle: Option<&str>,
    contract_size: Option<f64>,
) -> Option<Instrument> {
    use axon_core::types::{SpotInstrument, SwapInstrument, SwapSettle, Symbol};
    let base = Symbol::from(base);
    let quote = Symbol::from(quote);
    match kind {
        "spot" => Some(Instrument::Spot(SpotInstrument { base, quote })),
        "swap" => {
            let settle = match settle? {
                "usd_margin" | "UsdMargin" => SwapSettle::UsdMargin,
                "coin_margin" | "CoinMargin" => SwapSettle::CoinMargin,
                _ => return None,
            };
            Some(Instrument::Swap(SwapInstrument {
                base,
                quote,
                settle,
                contract_size: contract_size?,
            }))
        }
        _ => None,
    }
}

/// 0.6.0 helper:instrument 字符串 label(`"BTC/USDT"` / `"BTC/USDT:SWAP"`)
fn instrument_label(kind: &str, base: &str, quote: &str) -> String {
    match kind {
        "swap" => format!("{base}/{quote}:SWAP"),
        _ => format!("{base}/{quote}"),
    }
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

/// `MatchingEngineSnapshot` → Python `dict`
fn snapshot_to_dict<'py>(
    py: Python<'py>,
    snap: &RustMatchingEngineSnapshot,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("batch_mode", batch_mode_to_str(snap.batch_mode))?;
    d.set_item("timestamp_ns", snap.timestamp_ns)?;

    // engines: dict[instrument_label, L2Snapshot dict]
    let engines = PyDict::new(py);
    for (instrument, l2) in &snap.engines {
        let label = instrument.to_string();
        let l2_d = PyDict::new(py);
        l2_d.set_item(
            "instrument",
            instrument_to_dict(
                py,
                instrument.kind(),
                instrument.base().as_str(),
                instrument.quote().as_str(),
                match instrument {
                    Instrument::Swap(s) => Some(match s.settle {
                        axon_core::types::SwapSettle::UsdMargin => "usd_margin",
                        axon_core::types::SwapSettle::CoinMargin => "coin_margin",
                    }),
                    _ => None,
                },
                match instrument {
                    Instrument::Swap(s) => Some(s.contract_size),
                    _ => None,
                },
            )?,
        )?;
        l2_d.set_item("best_bid", l2.best_bid.map(|p| p.as_f64()))?;
        l2_d.set_item("best_ask", l2.best_ask.map(|p| p.as_f64()))?;
        l2_d.set_item("bid_depth", price_levels_to_pylist(py, &l2.bid_depth)?)?;
        l2_d.set_item("ask_depth", price_levels_to_pylist(py, &l2.ask_depth)?)?;
        l2_d.set_item("trade_count", l2.trade_count)?;
        engines.set_item(label, l2_d)?;
    }
    d.set_item("engines", engines)?;

    // cross_pairs: list[dict]
    let cps = PyList::empty(py);
    for cp in &snap.cross_pairs {
        let cp_d = PyDict::new(py);
        cp_d.set_item(
            "leg1",
            instrument_to_dict(
                py,
                cp.pair.spot.kind(),
                cp.pair.spot.base().as_str(),
                cp.pair.spot.quote().as_str(),
                match &cp.pair.spot {
                    Instrument::Swap(s) => Some(match s.settle {
                        axon_core::types::SwapSettle::UsdMargin => "usd_margin",
                        axon_core::types::SwapSettle::CoinMargin => "coin_margin",
                    }),
                    _ => None,
                },
                match &cp.pair.spot {
                    Instrument::Swap(s) => Some(s.contract_size),
                    _ => None,
                },
            )?,
        )?;
        cp_d.set_item(
            "leg2",
            instrument_to_dict(
                py,
                cp.pair.perp.kind(),
                cp.pair.perp.base().as_str(),
                cp.pair.perp.quote().as_str(),
                match &cp.pair.perp {
                    Instrument::Swap(s) => Some(match s.settle {
                        axon_core::types::SwapSettle::UsdMargin => "usd_margin",
                        axon_core::types::SwapSettle::CoinMargin => "coin_margin",
                    }),
                    _ => None,
                },
                match &cp.pair.perp {
                    Instrument::Swap(s) => Some(s.contract_size),
                    _ => None,
                },
            )?,
        )?;
        cp_d.set_item("ratio", cp.ratio)?;
        cp_d.set_item("max_quantity", cp.max_quantity.as_f64())?;
        cps.append(cp_d)?;
    }
    d.set_item("cross_pairs", cps)?;

    Ok(d)
}

/// Python `dict` → `MatchingEngineSnapshot`
fn snapshot_from_dict<'py>(
    _py: Python<'py>,
    snap: &Bound<'py, PyDict>,
) -> PyResult<RustMatchingEngineSnapshot> {
    use std::collections::HashMap;

    let batch_mode_str: String = require_dict_field(snap, "batch_mode")?;
    let batch_mode = parse_batch_mode(&batch_mode_str)?;
    let timestamp_ns: u64 = require_dict_field(snap, "timestamp_ns").unwrap_or(0);

    // engines: dict[instrument_label, L2Snapshot dict]
    let engines_obj: Bound<'py, PyDict> = require_dict_field(snap, "engines")?;
    let mut engines: HashMap<Instrument, _> = HashMap::new();
    for (key, value) in engines_obj.iter() {
        let label = key.extract::<String>()?;
        let _ = label; // 当前只用 key 位置做语义标记
        let l2_d = value
            .cast::<PyDict>()
            .map_err(|_e| PyValueError::new_err("snapshot.engines values must be dicts"))?;
        let inst_obj: Bound<'py, PyDict> = require_dict_field(l2_d, "instrument")?;
        let instrument = parse_instrument(&inst_obj)?;
        let _ = l2_d; // 暂时不恢复 depth
        engines.insert(
            instrument,
            crate::matching::l3::types::L2Snapshot {
                instrument: parse_instrument(&inst_obj)?,
                best_bid: None,
                best_ask: None,
                bid_depth: Vec::new(),
                ask_depth: Vec::new(),
                trade_count: 0,
            },
        );
    }

    // cross_pairs: list[dict] → Vec<CrossPair>
    let cps_obj: Bound<'py, PyList> = require_dict_field(snap, "cross_pairs")?;
    let mut cross_pairs: Vec<RustCrossPair> = Vec::with_capacity(cps_obj.len());
    for item in cps_obj.iter() {
        let cp_d = item
            .cast::<PyDict>()
            .map_err(|_e| PyValueError::new_err("snapshot.cross_pairs items must be dicts"))?;
        let pair = cross_pair_from_any(_py, cp_d.as_any())?;
        cross_pairs.push(pair);
    }

    Ok(RustMatchingEngineSnapshot {
        engines,
        cross_pairs,
        batch_mode,
        timestamp_ns,
    })
}

/// `L3Stats` → Python `dict`
fn l3_stats_to_dict<'py>(py: Python<'py>, stats: &RustL3Stats) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("total_assets", stats.total_assets)?;
    d.set_item("total_cross_fills", stats.total_cross_fills)?;
    d.set_item("total_batch_fills", stats.total_batch_fills)?;
    d.set_item("total_dark_fills", stats.total_dark_fills)?;
    d.set_item("total_arbitrage_profit", stats.total_arbitrage_profit)?;
    Ok(d)
}

/// 从 dict 提取必填字段(与 `super::types::require_field` 镜像,但不导出)
// 注:本函数保留以兼容其他模块可能引用,虽然主要使用 `require_dict_field`。
#[allow(dead_code)]
fn require_dict_field_legacy<'py, T>(dict: &Bound<'py, PyDict>, field: &str) -> PyResult<T>
where
    T: pyo3::conversion::FromPyObjectOwned<'py>,
{
    require_dict_field(dict, field)
}

// 保留 PyDict 在 doc-test 中需要
#[allow(dead_code)]
fn _py_dict_marker(_: &PyDict) {}

/// 字段缺失时返回空字符串)
#[allow(dead_code)]
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

    /// 构造 spot instrument dict 测试 helper
    fn spot_inst_dict<'py>(py: Python<'py>, base: &str, quote: &str) -> Bound<'py, PyDict> {
        let d = PyDict::new(py);
        d.set_item("kind", "spot").unwrap();
        d.set_item("base", base).unwrap();
        d.set_item("quote", quote).unwrap();
        d
    }

    /// 构造 swap instrument dict 测试 helper
    fn swap_inst_dict<'py>(
        _py: Python<'py>,
        base: &str,
        quote: &str,
        settle: &str,
        contract_size: f64,
    ) -> Bound<'py, PyDict> {
        let d = PyDict::new(py);
        d.set_item("kind", "swap").unwrap();
        d.set_item("base", base).unwrap();
        d.set_item("quote", quote).unwrap();
        d.set_item("settle", settle).unwrap();
        d.set_item("contract_size", contract_size).unwrap();
        d
    }

    fn make_limit_dict<'py>(
        _py: Python<'py>,
        id: u64,
        inst: &Bound<'py, PyDict>,
        side: &str,
        price: f64,
        qty: f64,
    ) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("id", id)?;
        d.set_item("instrument", inst)?;
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
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            m.register_instrument(py, &spot_inst_dict(py, "BTC", "USDT"))
                .unwrap();
            m.register_instrument(py, &spot_inst_dict(py, "ETH", "USDT"))
                .unwrap();
            m.set_batch_mode("auction").unwrap();
            let s = m.__repr__();
            assert!(s.contains("MultiAssetMatchingEngine"));
            assert!(s.contains("assets=2"));
            assert!(s.contains("batch_mode=auction"));
        });
    }

    /// 空 L3:资产 / 跨资产 / 模式 默认值
    #[test]
    fn empty_l3_defaults() {
        let m = PyMultiAssetMatchingEngine::new();
        assert_eq!(m.asset_count(), 0);
        assert_eq!(m.cross_pair_count(), 0);
        assert_eq!(m.batch_mode(), "continuous");
    }

    /// `register_instrument` 幂等
    #[test]
    fn register_instrument_idempotent() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            m.register_instrument(py, &spot_inst_dict(py, "BTC", "USDT"))
                .unwrap();
            m.register_instrument(py, &spot_inst_dict(py, "BTC", "USDT"))
                .unwrap();
            assert_eq!(m.asset_count(), 1);
        });
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
            let btc = spot_inst_dict(py, "BTC", "USDT");
            let eth = spot_inst_dict(py, "ETH", "USDT");
            m.register_instrument(py, &btc).unwrap();
            m.register_instrument(py, &eth).unwrap();

            // BTC:卖 @ 50000
            let sell = make_limit_dict(py, 1, &btc, "sell", 50000.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            // ETH:另一资产,无最优买价
            assert!(m.best_bid(py, &eth).unwrap().is_none());
            // BTC:最优卖价
            assert_eq!(m.best_ask(py, &btc).unwrap(), Some(50000.0));
        });
    }

    /// per-instrument 资产未注册时抛 `BacktestError(code="MatchingL3")`
    #[test]
    fn best_bid_unknown_instrument_raises_backtest_error() {
        Python::attach(|py| {
            let m = PyMultiAssetMatchingEngine::new();
            let unknown = spot_inst_dict(py, "UNKNOWN", "USDT");
            let err = m.best_bid(py, &unknown).unwrap_err();
            let s = err.to_string();
            assert!(
                s.contains("[MatchingL3]"),
                "expected [MatchingL3], got: {s}"
            );
        });
    }

    /// 连续模式下 submit 撮合正确
    #[test]
    fn submit_continuous_mode_yields_fill() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            let btc = spot_inst_dict(py, "BTC", "USDT");
            m.register_instrument(py, &btc).unwrap();
            let sell = make_limit_dict(py, 1, &btc, "sell", 100.0, 1.0).unwrap();
            m.submit(py, &sell).unwrap();
            let buy = make_limit_dict(py, 2, &btc, "buy", 100.0, 1.0).unwrap();
            let fills = m.submit(py, &buy).unwrap();
            assert_eq!(fills.len(), 1);
        });
    }

    /// 拍卖模式下 submit 暂存,run_auction 清算
    #[test]
    fn auction_mode_defers_orders() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            let eth = spot_inst_dict(py, "ETH", "USDT");
            m.register_instrument(py, &eth).unwrap();
            m.set_batch_mode("auction").unwrap();

            let buy = make_limit_dict(py, 1, &eth, "buy", 3000.0, 5.0).unwrap();
            let fills = m.submit(py, &buy).unwrap();
            assert_eq!(fills.len(), 0, "Auction 模式应暂存订单");

            let sell = make_limit_dict(py, 2, &eth, "sell", 3002.0, 5.0).unwrap();
            m.submit(py, &sell).unwrap();

            let result = m.run_auction(py, &eth).unwrap();
            assert!(result.has_trades());
            assert!(result.clearing_volume() > 0.0);
        });
    }

    /// 0.6.0 新增:spot + swap instrument 注册并存
    #[test]
    fn register_spot_and_swap_instruments() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            let btc_spot = spot_inst_dict(py, "BTC", "USDT");
            let btc_perp = swap_inst_dict(py, "BTC", "USDT", "usd_margin", 1.0);
            m.register_instrument(py, &btc_spot).unwrap();
            m.register_instrument(py, &btc_perp).unwrap();
            assert_eq!(m.asset_count(), 2, "spot + swap 应分别占 1 个 instrument");

            // spot 和 swap 的 best_ask 各自独立
            let spot_sell = make_limit_dict(py, 1, &btc_spot, "sell", 50000.0, 1.0).unwrap();
            m.submit(py, &spot_sell).unwrap();
            assert_eq!(m.best_ask(py, &btc_spot).unwrap(), Some(50000.0));
            assert!(m.best_ask(py, &btc_perp).unwrap().is_none());
        });
    }

    /// 0.6.0 新增:CrossPair 接受 instrument dict(spot + swap 配对)
    #[test]
    fn cross_pair_accepts_instrument_dicts() {
        Python::attach(|py| {
            let mut m = PyMultiAssetMatchingEngine::new();
            let btc_spot = spot_inst_dict(py, "BTC", "USDT");
            let btc_perp = swap_inst_dict(py, "BTC", "USDT", "usd_margin", 1.0);
            let pair = PyCrossPair::new(&btc_spot, &btc_perp, 1.0, 0.5).unwrap();
            m.register_cross_pair(py, &pair.as_any()).unwrap();
            assert_eq!(m.cross_pair_count(), 1);
            // 自动 register 两个 instrument
            assert_eq!(m.asset_count(), 2);
        });
    }
}
