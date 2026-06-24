//! Python 端 `OkxAdapter` —— 委托 `adapters::okx::OkxAdapter`。
//!
//! ## 与 Rust API 的关键差异
//!
//! - **同步包装**:Rust 端 `OkxAdapter` 所有方法都是 `async`(基于 tokio),
//!   Python 端用 `tokio::runtime::Runtime::block_on` 同步包装,符合 Python
//!   端无 asyncio 的调用习惯(同 `axon-data::python::PyDataService`)。
//!
//! - **Order 字典协议**:Python 端不直接构造 `Order` struct,通过 `dict`
//!   注入,内部 `dict_to_order` 转换(同 `binance.rs` 风格)。
//!   OKX 的 `clOrdId` 字段就是 `client_order_id`(UUID),与 Binance
//!   `newClientOrderId` 等价。
//!
//! - **类型输出字典化**:`get_balance` / `get_positions` / `get_ticker` /
//!   `get_depth` / 合约相关方法返回 `dict` / `list[dict]`,Decimal 字段
//!   全部用 `str` 表示(精度无损)。
//!
//! - **`&mut self.inner` 方法用 `PyRefMut<Self>` 包装**:同 `binance.rs`,
//!   通过 destructure 把 `slf` 拆成 `inner` + `rt` 两个独立借用,避免
//!   `&mut self.inner` 与 `self.rt.block_on(&self)` 同时借用冲突。
//!
//! - **安全**:`ExchangeConfig.api_secret` 不暴露到 `__repr__`(详见
//!   `config.rs`);调用方应使用 `python/axon_quant/exchange.py` 提供的
//!   `okx_testnet_config()` 工厂从环境变量读 key + passphrase。
//!
//! ## 当前实现覆盖
//!
//! - `connect` / `disconnect` / `subscribe` / `place_order` / `cancel_order`
//! - `get_balance` / `get_positions` / `get_depth` / `get_ticker`
//! - `set_leverage` / `set_margin_type` / `get_leverage_brackets` /
//!   `set_position_mode` / `get_funding_rate` / `get_account_info` /
//!   `get_open_interest` / `get_long_short_ratio`
//!
//! ## OKX 特有差异
//!
//! - **passphrase**:OKX 必须,通过 `ExchangeConfig.passphrase` 传入
//!   (已在 `config.rs` 暴露 getter)
//! - **symbol 格式**:OKX 用 `"BTC-USDT"`(中划线)而非 Binance 的 `"BTCUSDT"`
//!   (无中划线);`dict_to_order` 不强制,直接透传字符串
//! - **撤单需 instId**:OKX 撤单 API 需要 `instId` + `clOrdId`,
//!   内部维护 `clOrdId -> instId` 映射,`place_order` 时写入,
//!   `cancel_order` 时查找;若找不到返回 `OrderNotFound`

use std::str::FromStr;
use std::sync::Arc;

use pyo3::PyRefMut;
use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use rust_decimal::Decimal;
use tokio::runtime::Runtime;

use axon_core::dict_field;
use axon_core::parse_py_enum;

use crate::adapters::okx::OkxAdapter;
use crate::traits::ExchangeAdapter;
use crate::types::{
    MarginType as RustMarginType, Order as RustOrder, OrderId as RustOrderId,
    OrderType as RustOrderType, Side as RustSide, Symbol as RustSymbol, TimeInForce as RustTif,
};

use super::config::PyExchangeConfig;
use super::error::to_py_err;

// ═══════════════════════════════════════════════════════════════════════════
// 主类: PyOkxAdapter
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `OkxAdapter` —— OKX 现货 / 合约 REST + WebSocket。
///
/// 内部持有 `OkxAdapter` 与一个 current-thread tokio Runtime,
/// 所有 async 方法走 `rt.block_on` 同步包装。
///
/// **生命周期**:用户用 `OkxAdapter(config)` 构造;`connect` 启动
/// WebSocket 监督任务;`disconnect` 优雅关闭。用完应 `disconnect()` 释放
/// 网络资源。
///
/// **passphrase**:构造时需在 `ExchangeConfig.passphrase` 传入 OKX
/// passphrase(可通过 `okx_testnet_config()` 工厂从 `OKX_PASSPHRASE`
/// 环境变量读)。
///
/// `skip_from_py_object`:Python 端不传 `OkxAdapter` 实例给其他
/// Python 函数(只通过构造 + 调方法使用);`__init__` 收 `ExchangeConfig`
/// 已经通过 `from_py_object` 实现。
#[pyclass(name = "OkxAdapter", skip_from_py_object)]
pub struct PyOkxAdapter {
    /// Rust 端 `OkxAdapter`(持有 config + client + ws supervisor + order_inst_ids 映射)
    inner: OkxAdapter,
    /// Tokio current-thread 运行时(`block_on` 包装 async API)
    rt: Arc<Runtime>,
}

#[pymethods]
impl PyOkxAdapter {
    /// 构造一个未连接的 `OkxAdapter`。
    ///
    /// 构造后**必须**调用 `connect()` 才会启动 WebSocket 监督任务。
    /// 多次 `connect()` / `disconnect()` 切换安全。
    #[new]
    fn new(config: PyExchangeConfig) -> PyResult<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("tokio: {e}")))?;
        Ok(Self {
            inner: OkxAdapter::new(config.inner),
            rt: Arc::new(rt),
        })
    }

    /// 返回交易所 ID(`"okx"`)。
    #[getter]
    fn exchange_id(&self) -> String {
        "okx".to_string()
    }

    /// 连接:ping REST `/api/v5/public/time` + 启动 WebSocket 监督任务。
    ///
    /// **错误**:返回 `ExchangeError`(`ConnectionFailed` / `ApiError` / `Network`)。
    fn connect<'py>(mut slf: PyRefMut<'py, Self>) -> PyResult<()> {
        // destructure 把 slf.inner / slf.rt 拆成独立借用
        let Self { inner, rt } = &mut *slf;
        rt.block_on(async move { inner.connect().await })
            .map_err(to_py_err)
    }

    /// 断开连接:关闭 WebSocket 监督任务。
    fn disconnect<'py>(mut slf: PyRefMut<'py, Self>) -> PyResult<()> {
        let Self { inner, rt } = &mut *slf;
        rt.block_on(async move { inner.disconnect().await })
            .map_err(to_py_err)
    }

    /// 订阅行情:WebSocket 订阅 Tickers / books5 / trades 多路流。
    ///
    /// Args:
    /// - `symbols`: 交易对列表(OKX 格式,如 `["BTC-USDT", "ETH-USDT"]`)
    ///
    /// **注意**:订阅需要在 `connect()` 之后调用;若 WebSocket 尚未就绪,
    /// 订阅信息会缓存在内部,重连后自动重发(去重)。
    fn subscribe<'py>(mut slf: PyRefMut<'py, Self>, symbols: Vec<String>) -> PyResult<()> {
        let rust_symbols: Vec<RustSymbol> = symbols.into_iter().map(RustSymbol::new).collect();
        let Self { inner, rt } = &mut *slf;
        rt.block_on(async move { inner.subscribe(&rust_symbols).await })
            .map_err(to_py_err)
    }

    /// 下单:接受 dict,返回 `order_id` (UUID 字符串)。
    ///
    /// dict 必填字段:
    /// - `symbol` (str): 交易对(OKX 格式,如 `"BTC-USDT"`)
    /// - `side` (str): `"buy"` / `"sell"`
    /// - `type` (str): `"market"` / `"limit"`
    /// - `quantity` (str/Decimal): 下单数量
    /// - `tif` (str): `"GTC"` / `"IOC"` / `"FOK"`
    ///
    /// dict 可选字段:
    /// - `price` (str/Decimal,Optional): 限价单必填
    /// - `client_order_id` (str,Optional): 客户端订单 ID(UUID 字符串),
    ///   OKX 端映射为 `clOrdId`;缺省时自动生成
    /// - `meta` (dict,Optional): 透传给交易所的元数据
    ///
    /// **错误**:`ConnectionFailed` (未连接) / `OrderRejected` /
    /// `InsufficientBalance` / `ApiError` / `ParseError`。
    fn place_order<'py>(
        mut slf: PyRefMut<'py, Self>,
        order_dict: &Bound<'py, PyDict>,
    ) -> PyResult<String> {
        // 与 Binance 共用 dict → Order 协议,exchange 字段写 OKX
        let rust_order = dict_to_order(order_dict, crate::types::ExchangeId::Okx)?;
        let Self { inner, rt } = &mut *slf;
        let order_id = rt
            .block_on(async move { inner.send_order(rust_order).await })
            .map_err(to_py_err)?;
        Ok(order_id.to_string())
    }

    /// 取消订单。
    ///
    /// Args:
    /// - `order_id` (str): 订单 ID(UUID 字符串,对应 OKX `clOrdId`)
    ///
    /// **注意**:OKX 撤单需要 `instId` + `clOrdId`,我们通过
    /// `client_order_id` → `instId` 内部映射查找;若该订单不在本地映射,
    /// 返回 `OrderNotFound`。
    fn cancel_order<'py>(mut slf: PyRefMut<'py, Self>, order_id: &str) -> PyResult<()> {
        let oid = parse_order_id(order_id)?;
        let Self { inner, rt } = &mut *slf;
        rt.block_on(async move { inner.cancel_order(oid).await })
            .map_err(to_py_err)
    }

    /// 查询余额:返回 `dict[currency, dict]`。
    ///
    /// dict 值字段:
    /// - `currency` (str): 币种
    /// - `available` (str,Decimal): 可用余额
    /// - `locked` (str,Decimal): 冻结余额
    fn get_balance<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let balances = self
            .rt
            .block_on(async move { self.inner.get_balance().await })
            .map_err(to_py_err)?;
        let d = PyDict::new(py);
        for (currency, bal) in &balances {
            d.set_item(currency, balance_to_dict(py, bal)?)?;
        }
        Ok(d)
    }

    /// 查询持仓:返回 `list[dict]`(OKX 标准持仓格式)。
    fn get_positions<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let positions = self
            .rt
            .block_on(async move { self.inner.get_positions().await })
            .map_err(to_py_err)?;
        let list = PyList::empty(py);
        for p in &positions {
            list.append(position_to_dict(py, p)?)?;
        }
        Ok(list)
    }

    /// 同步读深度缓存(WebSocket 收到 books5 时更新)。
    ///
    /// `None` 表示该 symbol 还没有深度数据。
    fn get_depth<'py>(
        &self,
        py: Python<'py>,
        symbol: &str,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let rust_symbol = RustSymbol::new(symbol);
        let snap = self.inner.get_depth(&rust_symbol);
        match snap {
            Some(s) => Ok(Some(depth_snapshot_to_dict(py, &s)?)),
            None => Ok(None),
        }
    }

    /// 同步读 Ticker 缓存(WebSocket 收到 tickers 时更新)。
    fn get_ticker<'py>(
        &self,
        py: Python<'py>,
        symbol: &str,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let rust_symbol = RustSymbol::new(symbol);
        let ticker = self.inner.get_ticker(&rust_symbol);
        match ticker {
            Some(t) => Ok(Some(ticker_to_dict(py, &t)?)),
            None => Ok(None),
        }
    }

    // === 合约 / 杠杆(Stage 4' D) ===

    /// 设置杠杆倍数(OKX 范围 1-125)。
    fn set_leverage(&self, symbol: &str, leverage: u8) -> PyResult<()> {
        let sym = symbol.to_string();
        self.rt
            .block_on(async move { self.inner.set_leverage(&sym, leverage).await })
            .map_err(to_py_err)
    }

    /// 设置保证金模式(`"isolated"` / `"cross"`)。
    fn set_margin_type(&self, symbol: &str, margin_type: &str) -> PyResult<()> {
        let mt = parse_margin_type(margin_type)?;
        let sym = symbol.to_string();
        self.rt
            .block_on(async move { self.inner.set_margin_type(&sym, mt).await })
            .map_err(to_py_err)
    }

    /// 获取杠杆分层。
    fn get_leverage_brackets<'py>(
        &self,
        py: Python<'py>,
        symbol: &str,
    ) -> PyResult<Bound<'py, PyList>> {
        let sym = symbol.to_string();
        let brackets = self
            .rt
            .block_on(async move { self.inner.get_leverage_brackets(&sym).await })
            .map_err(to_py_err)?;
        let list = PyList::empty(py);
        for b in &brackets {
            let d = PyDict::new(py);
            d.set_item("bracket", b.bracket)?;
            d.set_item("min_leverage", b.min_leverage)?;
            d.set_item("max_leverage", b.max_leverage)?;
            d.set_item("max_notional", b.max_notional.to_string())?;
            d.set_item("maint_margin_ratio", b.maint_margin_ratio.to_string())?;
            list.append(d)?;
        }
        Ok(list)
    }

    /// 设置持仓模式(`true`=对冲 hedge `long_short_mode`,`false`=单向 net `net_mode`)。
    fn set_position_mode(&self, hedge_mode: bool) -> PyResult<()> {
        self.rt
            .block_on(async move { self.inner.set_position_mode(hedge_mode).await })
            .map_err(to_py_err)
    }

    /// 获取资金费率(SWAP 合约)。
    fn get_funding_rate<'py>(&self, py: Python<'py>, symbol: &str) -> PyResult<Bound<'py, PyDict>> {
        let sym = symbol.to_string();
        let rate = self
            .rt
            .block_on(async move { self.inner.get_funding_rate(&sym).await })
            .map_err(to_py_err)?;
        let d = PyDict::new(py);
        d.set_item("symbol", &rate.symbol)?;
        d.set_item("rate", rate.rate.to_string())?;
        d.set_item("next_funding_ms", rate.next_funding_ms)?;
        d.set_item("mark_price", rate.mark_price.to_string())?;
        d.set_item("index_price", rate.index_price.to_string())?;
        Ok(d)
    }

    /// 获取完整账户信息(余额 + 盈亏 + 保证金 + 持仓模式)。
    fn get_account_info<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let info = self
            .rt
            .block_on(async move { self.inner.get_account_info().await })
            .map_err(to_py_err)?;
        account_info_to_dict(py, &info)
    }

    /// 获取未平仓合约数。
    fn get_open_interest<'py>(
        &self,
        py: Python<'py>,
        symbol: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let sym = symbol.to_string();
        let oi = self
            .rt
            .block_on(async move { self.inner.get_open_interest(&sym).await })
            .map_err(to_py_err)?;
        let d = PyDict::new(py);
        d.set_item("symbol", &oi.symbol)?;
        d.set_item("contracts", oi.contracts)?;
        d.set_item("notional", oi.notional.to_string())?;
        d.set_item("timestamp_ms", oi.timestamp_ms)?;
        Ok(d)
    }

    /// 获取多空账户比。
    fn get_long_short_ratio<'py>(
        &self,
        py: Python<'py>,
        symbol: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let sym = symbol.to_string();
        let r = self
            .rt
            .block_on(async move { self.inner.get_long_short_ratio(&sym).await })
            .map_err(to_py_err)?;
        let d = PyDict::new(py);
        d.set_item("symbol", &r.symbol)?;
        d.set_item("long_ratio", r.long_ratio)?;
        d.set_item("short_ratio", r.short_ratio)?;
        d.set_item("long_short_ratio", r.long_short_ratio)?;
        d.set_item("timestamp_ms", r.timestamp_ms)?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        "OkxAdapter(...)".to_string()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// dict ↔ Order / 输出字典化 helper(与 binance.rs 风格一致,exchange 字段写 OKX)
// ═══════════════════════════════════════════════════════════════════════════

/// Python dict → Rust [`RustOrder`](OKX 模式)。
///
/// 必填:`symbol` / `side` / `type` / `quantity` / `tif`
/// 可选:`price`(限价单必填)/ `client_order_id`(UUID str,缺省自动生成)/
///      `meta`(dict,OKX 可透传 `tag` 等额外字段)
fn dict_to_order(
    dict: &Bound<'_, PyDict>,
    exchange: crate::types::ExchangeId,
) -> PyResult<RustOrder> {
    let symbol: String = dict_field!(dict, "symbol", String);
    let side_str: String = dict_field!(dict, "side", String);
    let side = parse_side(&side_str)?;
    let type_str: String = dict_field!(dict, "type", String);
    let qty_any: Bound<'_, PyAny> = dict
        .get_item("quantity")?
        .ok_or_else(|| PyKeyError::new_err("missing 'quantity'"))?;
    let quantity = py_to_decimal(&qty_any)?;
    let tif_str: String = dict_field!(dict, "tif", String);
    let time_in_force = parse_tif(&tif_str)?;

    // price: 可选;限价单必填
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

    // order_type + 校验 price 一致性
    // 注:OKX 官方支持 market / limit;stop_loss / stop_limit 在 OKX 用
    // `orderToOrderType=conditional` 模式,Stage 5 不暴露,限制到 market/limit
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
                "OKX unsupported order type: {other} (supported: market / limit)"
            )));
        }
    };

    // meta: 可选 dict[str, str]
    let mut meta = std::collections::HashMap::new();
    if let Some(m) = dict.get_item("meta")? {
        let m_dict: &Bound<'_, PyDict> = m.cast()?;
        for (k, v) in m_dict.iter() {
            let ks: String = k.extract()?;
            let vs: String = v.extract()?;
            meta.insert(ks, vs);
        }
    }

    Ok(RustOrder {
        client_order_id,
        symbol: RustSymbol::new(symbol),
        side,
        order_type,
        price,
        quantity,
        time_in_force,
        exchange,
        meta,
    })
}

/// Rust `AccountBalance` → Python dict
fn balance_to_dict<'py>(
    py: Python<'py>,
    b: &crate::types::AccountBalance,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("currency", &b.currency)?;
    d.set_item("available", b.available.to_string())?;
    d.set_item("locked", b.locked.to_string())?;
    Ok(d)
}

/// Rust `Position` → Python dict
fn position_to_dict<'py>(
    py: Python<'py>,
    p: &crate::types::Position,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("symbol", p.symbol.to_string())?;
    d.set_item("side", format!("{:?}", p.side))?;
    d.set_item("quantity", p.quantity.to_string())?;
    d.set_item("avg_entry_price", p.avg_entry_price.to_string())?;
    d.set_item("unrealized_pnl", p.unrealized_pnl.to_string())?;
    Ok(d)
}

/// Rust `Ticker` → Python dict
fn ticker_to_dict<'py>(py: Python<'py>, t: &crate::types::Ticker) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("symbol", t.symbol.to_string())?;
    d.set_item("bid", t.bid.to_string())?;
    d.set_item("ask", t.ask.to_string())?;
    d.set_item("last", t.last.to_string())?;
    d.set_item("volume_24h", t.volume_24h.to_string())?;
    d.set_item("timestamp", t.timestamp.to_rfc3339())?;
    Ok(d)
}

/// Rust `DepthSnapshot` → Python dict
fn depth_snapshot_to_dict<'py>(
    py: Python<'py>,
    s: &crate::types::DepthSnapshot,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("symbol", s.symbol.to_string())?;
    d.set_item("bids", depth_levels_to_list(py, &s.bids)?)?;
    d.set_item("asks", depth_levels_to_list(py, &s.asks)?)?;
    d.set_item("timestamp", s.timestamp.to_rfc3339())?;
    Ok(d)
}

/// 深度层 `Vec<(Decimal, Decimal)>` → Python `list[[price, qty]]`
fn depth_levels_to_list<'py>(
    py: Python<'py>,
    levels: &[(Decimal, Decimal)],
) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for (price, qty) in levels {
        let pair = PyList::empty(py);
        pair.append(price.to_string())?;
        pair.append(qty.to_string())?;
        list.append(pair)?;
    }
    Ok(list)
}

/// Rust `AccountInfo` → Python dict
fn account_info_to_dict<'py>(
    py: Python<'py>,
    info: &crate::types::AccountInfo,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("total_balance", info.total_balance.to_string())?;
    d.set_item("available_balance", info.available_balance.to_string())?;
    d.set_item("unrealized_pnl", info.unrealized_pnl.to_string())?;
    d.set_item("margin_used", info.margin_used.to_string())?;
    d.set_item("initial_margin", info.initial_margin.to_string())?;
    d.set_item("maintenance_margin", info.maintenance_margin.to_string())?;
    d.set_item("position_mode", format!("{:?}", info.position_mode))?;
    d.set_item("as_of_ms", info.as_of_ms)?;
    Ok(d)
}

// ═══════════════════════════════════════════════════════════════════════════
// 解析 helper(与 binance.rs 风格一致)
// ═══════════════════════════════════════════════════════════════════════════

/// Python `Decimal` / `int` / `float` / `str` → Rust `Decimal`(精度无损)
fn py_to_decimal(obj: &Bound<'_, PyAny>) -> PyResult<Decimal> {
    let s: String = obj.call_method0("__str__")?.extract()?;
    Decimal::from_str(&s)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid decimal: {e}")))
}

parse_py_enum!(parse_side, RustSide, [
    Buy => "buy",
    Sell => "sell",
]);

parse_py_enum!(parse_tif, RustTif, [
    Gtc => "gtc",
    Ioc => "ioc",
    Fok => "fok",
]);

/// `margin_type` 字符串解析
fn parse_margin_type(s: &str) -> PyResult<RustMarginType> {
    match s.to_lowercase().as_str() {
        "isolated" => Ok(RustMarginType::Isolated),
        "cross" => Ok(RustMarginType::Cross),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "invalid margin type: {other} (expected 'isolated' or 'cross')"
        ))),
    }
}

/// Python 端 `order_id` (str) → Rust `OrderId`
fn parse_order_id(s: &str) -> PyResult<RustOrderId> {
    uuid::Uuid::from_str(s)
        .map(RustOrderId)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid order id: {e}")))
}

// ═══════════════════════════════════════════════════════════════════════════
// 注册
// ═══════════════════════════════════════════════════════════════════════════

/// 在 `_native.exchange` 下注册 `OkxAdapter`
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyOkxAdapter>()
}

// ═══════════════════════════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ExchangeId;
    use rust_decimal_macros::dec;

    /// 测试用 OKX testnet 配置(带 passphrase)
    ///
    /// 直接构造 `PyExchangeConfig { inner: ... }`,避免依赖 `#[pymethods]`
    /// 的 `new` 方法可见性(`#[new]` 只对 Python 端可见,Rust 端调用需要
    /// 单独的 `pub fn`)。inner 字段是 `pub` 允许这样做。
    fn test_config() -> PyExchangeConfig {
        use crate::types::{
            ExchangeId as RustId, RateLimitConfig as RustRate, ReconnectConfig as RustReconnect,
        };
        use std::time::Duration;
        PyExchangeConfig {
            inner: crate::types::ExchangeConfig {
                exchange_id: RustId::Okx,
                api_key: "test_key".into(),
                api_secret: "test_secret".into(),
                passphrase: Some("test_pass".into()),
                testnet: true,
                rest_base_url: "https://www.okx.com".into(),
                ws_url: "wss://wspap.okx.com:8443/ws/v5/public?brokerId=9999".into(),
                rate_limit: RustRate {
                    requests_per_second: 20,
                    orders_per_minute: 60,
                    ws_messages_per_second: 50,
                },
                reconnect: RustReconnect {
                    max_retries: 1,
                    initial_backoff: Duration::from_millis(10),
                    max_backoff: Duration::from_millis(100),
                    backoff_multiplier: 2.0,
                    circuit_breaker_threshold: 1,
                    circuit_breaker_reset: Duration::from_secs(60),
                },
                proxy: None,
                position_endpoint: "/api/v5/account/positions".into(),
                fapi_base_url: None,
            },
        }
    }

    /// 构造 + `__repr__` 不泄漏 + 交易所 ID 正确
    #[test]
    fn okx_adapter_construct_and_repr() {
        let adapter = PyOkxAdapter::new(test_config()).unwrap();
        let r = adapter.__repr__();
        assert!(r.contains("OkxAdapter"), "got: {r}");
        // 关键:__repr__ 不含 api_secret / api_key
        assert!(!r.contains("test_secret"), "repr leaked secret: {r}");
        assert!(!r.contains("test_key"), "repr leaked key: {r}");
        assert_eq!(adapter.exchange_id(), "okx");
    }

    /// 限价单 dict 解析正确(OKX symbol 格式 `BTC-USDT`)
    #[test]
    fn dict_to_order_limit_full() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "limit").unwrap();
            d.set_item("quantity", "0.1").unwrap();
            d.set_item("price", "50000").unwrap();
            d.set_item("tif", "GTC").unwrap();
            let order = dict_to_order(&d, ExchangeId::Okx).unwrap();
            assert_eq!(order.symbol, RustSymbol::new("BTC-USDT"));
            assert_eq!(order.side, RustSide::Buy);
            assert!(matches!(order.order_type, RustOrderType::Limit));
            assert_eq!(order.quantity, dec!(0.1));
            assert_eq!(order.price, Some(dec!(50000)));
            assert_eq!(order.exchange, ExchangeId::Okx);
        });
    }

    /// 市价单 dict 不需要 price,带 price 会报错
    #[test]
    fn dict_to_order_market_no_price() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("symbol", "ETH-USDT").unwrap();
            d.set_item("side", "sell").unwrap();
            d.set_item("type", "market").unwrap();
            d.set_item("quantity", "1.5").unwrap();
            d.set_item("tif", "IOC").unwrap();
            let order = dict_to_order(&d, ExchangeId::Okx).unwrap();
            assert!(matches!(order.order_type, RustOrderType::Market));
            assert!(order.price.is_none());
        });
    }

    /// OKX 不支持 stop_loss / stop_limit → PyValueError
    #[test]
    fn dict_to_order_unsupported_type_raises() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "stop_loss").unwrap();
            d.set_item("quantity", "0.1").unwrap();
            d.set_item("price", "50000").unwrap();
            d.set_item("tif", "GTC").unwrap();
            let err = dict_to_order(&d, ExchangeId::Okx).unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyValueError>(py));
        });
    }

    /// 限价单缺 price → PyKeyError
    #[test]
    fn dict_to_order_limit_missing_price_raises() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "limit").unwrap();
            d.set_item("quantity", "0.1").unwrap();
            d.set_item("tif", "GTC").unwrap();
            let err = dict_to_order(&d, ExchangeId::Okx).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
        });
    }

    /// 非法 side 字符串 → PyValueError
    #[test]
    fn dict_to_order_invalid_side_raises() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "XXX").unwrap();
            d.set_item("type", "market").unwrap();
            d.set_item("quantity", "0.1").unwrap();
            d.set_item("tif", "GTC").unwrap();
            let err = dict_to_order(&d, ExchangeId::Okx).unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyValueError>(py));
        });
    }

    /// 缺必填字段 → PyKeyError
    #[test]
    fn dict_to_order_missing_required_field_raises() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "buy").unwrap();
            // 缺 type / quantity / tif
            let err = dict_to_order(&d, ExchangeId::Okx).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
        });
    }

    /// `parse_order_id` 合法 UUID → 成功
    #[test]
    fn parse_order_id_valid_uuid() {
        let oid = parse_order_id("00000000-0000-0000-0000-000000000000").unwrap();
        assert_eq!(oid.to_string(), "00000000-0000-0000-0000-000000000000");
    }

    /// `parse_order_id` 非法字符串 → PyValueError
    #[test]
    fn parse_order_id_invalid_str_raises() {
        Python::attach(|py| {
            let err = parse_order_id("not-a-uuid").unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyValueError>(py));
        });
    }

    /// `parse_side` 大小写不敏感
    #[test]
    fn parse_side_case_insensitive() {
        assert_eq!(parse_side("buy").unwrap(), RustSide::Buy);
        assert_eq!(parse_side("BUY").unwrap(), RustSide::Buy);
        assert_eq!(parse_side("Sell").unwrap(), RustSide::Sell);
        assert!(parse_side("xxx").is_err());
    }

    /// `parse_tif` 大小写不敏感
    #[test]
    fn parse_tif_case_insensitive() {
        assert_eq!(parse_tif("GTC").unwrap(), RustTif::Gtc);
        assert_eq!(parse_tif("ioc").unwrap(), RustTif::Ioc);
        assert!(parse_tif("GTD").is_err());
    }

    /// `parse_margin_type` 大小写不敏感
    #[test]
    fn parse_margin_type_case_insensitive() {
        assert_eq!(
            parse_margin_type("isolated").unwrap(),
            RustMarginType::Isolated
        );
        assert_eq!(parse_margin_type("Cross").unwrap(), RustMarginType::Cross);
        assert!(parse_margin_type("xxx").is_err());
    }

    /// `py_to_decimal` 通过 Python 路径精度无损
    #[test]
    fn py_to_decimal_via_python() {
        Python::attach(|py| {
            let d = py
                .import("decimal")
                .unwrap()
                .call_method1("Decimal", ("0.1",))
                .unwrap();
            let v = py_to_decimal(&d).unwrap();
            assert_eq!(v, dec!(0.1));
        });
    }

    /// `py_to_decimal` 无效字符串 → PyValueError
    #[test]
    fn py_to_decimal_invalid_raises() {
        Python::attach(|py| {
            let none = py.None().into_bound(py);
            let err = py_to_decimal(&none).unwrap_err();
            assert!(err.is_instance_of::<pyo3::exceptions::PyValueError>(py));
        });
    }

    /// `register` 函数签名稳定
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
