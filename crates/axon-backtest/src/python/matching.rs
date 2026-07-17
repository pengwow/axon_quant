//! Python 端 MatchingEngine 桥接
//!
//! 让任意 Python 类(包括 ImpactedMatchingEngine / L2MatchingEngine)通过
//! PyO3 实现 MatchingEngine Rust trait,被 BacktestEngine.with_matching_engine 真注入。
//!
//! # dict 协议
//!
//! 跟 `python::types::dict_to_order` / `submit_result_to_dict` 对称:
//! - `Order → dict` 字段:`id / symbol / side / type / quantity / tif / price(限价单)`
//! - Python `submit(dict) → dict` 返回字段:`fills / is_filled / is_partially_filled / remaining_quantity`

use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use axon_core::dict_field;
use axon_core::market::Side;
use axon_core::order::{Order, OrderId, OrderType, TimeInForce};
use axon_core::types::{Price, Quantity, Symbol};

use crate::matching::engine::MatchingEngine;
use crate::matching::types::{MatchFill, SubmitResult};

/// Python 端撮合引擎的 Rust 桥接
///
/// 通过 `Arc<Mutex<Py<PyAny>>>` 持有 Python 对象引用,
/// `submit` 时 acquire GIL + 调 `py_engine.submit(order_to_dict(&order))`,
/// 把返回的 dict 转换成 Rust `SubmitResult`。
#[derive(Clone)]
pub struct PyMatchingEngine {
    inner: Arc<Mutex<Py<PyAny>>>,
}

impl PyMatchingEngine {
    /// 构造桥接 — 校验 Python 对象有 `submit` 方法
    pub fn new(py_engine: &Bound<'_, PyAny>) -> PyResult<Self> {
        if py_engine.getattr("submit").is_err() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "matching engine must implement submit(Order dict) -> dict",
            ));
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(py_engine.clone().unbind())),
        })
    }
}

// ─── Order → Python dict ───────────────────────────────────

/// Rust `Order` 转 Python dict(供 Python `submit` 接收)
pub fn order_to_dict<'py>(py: Python<'py>, order: &Order) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("id", order.id)?;
    d.set_item("symbol", order.symbol.as_str())?;
    d.set_item("side", side_str(order.side))?;
    d.set_item("quantity", order.quantity.as_f64())?;
    d.set_item("tif", tif_str(order.time_in_force))?;
    match order.order_type {
        OrderType::Limit { price } => {
            d.set_item("type", "limit")?;
            d.set_item("price", price.as_f64())?;
        }
        OrderType::Market => {
            d.set_item("type", "market")?;
        }
        // ponytail: Stop/StopLimit/Iceberg 在回测场景用不到,转 limit/market 兜底
        OrderType::Stop { .. } | OrderType::StopLimit { .. } => {
            d.set_item("type", "market")?;
        }
        OrderType::Iceberg { .. } => {
            d.set_item("type", "limit")?;
        }
    }
    Ok(d)
}

fn side_str(side: Side) -> &'static str {
    match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

fn tif_str(tif: TimeInForce) -> &'static str {
    match tif {
        TimeInForce::GTC => "GTC",
        TimeInForce::IOC => "IOC",
        TimeInForce::FOK => "FOK",
        TimeInForce::GFD => "GFD",
        TimeInForce::FAK => "FAK",
    }
}

// ─── Python dict → SubmitResult ──────────────────────────────

/// Python `submit` 返回的 dict 转 Rust `SubmitResult`
fn submit_result_from_dict(dict: &Bound<'_, PyDict>) -> PyResult<SubmitResult> {
    let fills_py = dict
        .get_item("fills")?
        .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err("missing 'fills'"))?;
    let fills_list = fills_py.cast::<PyList>()?;

    let mut fills = Vec::with_capacity(fills_list.len());
    for fill_any in fills_list.iter() {
        let fill_dict = fill_any.cast::<PyDict>()?;
        fills.push(match_fill_from_dict(fill_dict)?);
    }

    let is_filled: bool = dict
        .get_item("is_filled")?
        .and_then(|v| v.extract::<bool>().ok())
        .unwrap_or(false);
    let is_partially_filled: bool = dict
        .get_item("is_partially_filled")?
        .and_then(|v| v.extract::<bool>().ok())
        .unwrap_or(false);
    let remaining_quantity: f64 = dict
        .get_item("remaining_quantity")?
        .and_then(|v| v.extract::<f64>().ok())
        .unwrap_or(0.0);

    Ok(SubmitResult {
        fills,
        is_filled,
        is_partially_filled,
        remaining_quantity: Quantity::from_f64(remaining_quantity),
    })
}

/// Python `MatchFill` dict 转 Rust `MatchFill`
fn match_fill_from_dict(dict: &Bound<'_, PyDict>) -> PyResult<MatchFill> {
    let fill_id: u64 = dict_field!(dict, "fill_id", u64);
    let taker_order_id: u64 = dict_field!(dict, "taker_order_id", u64);
    let maker_order_id: u64 = dict_field!(dict, "maker_order_id", u64);
    let price: f64 = dict_field!(dict, "price", f64);
    let quantity: f64 = dict_field!(dict, "quantity", f64);
    let taker_side_str: String = dict_field!(dict, "taker_side", String);
    let taker_side = parse_side(&taker_side_str)?;
    let timestamp: i64 = dict
        .get_item("timestamp")
        .ok()
        .flatten()
        .and_then(|v| v.extract::<i64>().ok())
        .unwrap_or(0);

    Ok(MatchFill {
        fill_id,
        taker_order_id,
        maker_order_id,
        price: Price::from_f64(price),
        quantity: Quantity::from_f64(quantity),
        taker_side,
        timestamp: axon_core::time::Timestamp::from_nanos(timestamp),
    })
}

fn parse_side(s: &str) -> PyResult<Side> {
    match s.to_lowercase().as_str() {
        "buy" => Ok(Side::Buy),
        "sell" => Ok(Side::Sell),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "invalid side: {other}"
        ))),
    }
}

// ─── MatchingEngine trait 实现 ──────────────────────────────

impl MatchingEngine for PyMatchingEngine {
    fn submit(&mut self, order: Order) -> SubmitResult {
        // ponytail: Python 端异常降级为空结果(不阻塞整个 backtest)
        match self.try_submit(&order) {
            Ok(r) => r,
            Err(e) => {
                Python::attach(|py| e.print(py));
                SubmitResult::empty(Quantity::from_f64(0.0))
            }
        }
    }

    fn cancel(&mut self, _order_id: OrderId) -> bool {
        false
    }

    fn best_bid(&self) -> Option<Price> {
        None
    }

    fn best_ask(&self) -> Option<Price> {
        None
    }

    fn spread(&self) -> Option<Price> {
        None
    }

    fn depth(
        &self,
        _levels: usize,
    ) -> (
        Vec<crate::matching::types::OrderBookLevel>,
        Vec<crate::matching::types::OrderBookLevel>,
    ) {
        (Vec::new(), Vec::new())
    }

    fn active_order_count(&self) -> usize {
        Python::attach(|py| {
            let engine = match self.inner.lock() {
                Ok(g) => g,
                Err(_) => return 0,
            };
            let py_engine = engine.bind(py);
            py_engine
                .getattr("active_order_count")
                .and_then(|m| m.call0())
                .and_then(|r| r.extract::<usize>())
                .unwrap_or(0)
        })
    }

    fn clear_book(&mut self) {
        let _ = Python::attach(|py| {
            let engine = match self.inner.lock() {
                Ok(g) => g,
                Err(_) => return Ok(()),
            };
            let py_engine = engine.bind(py);
            py_engine.call_method0("clear_book").map(|_| ())
        });
    }

    /// 虚拟流动性种子(透传到 Python 端 `seed_liquidity` 方法)
    ///
    /// 行为约定:
    /// - Python 对象有 `seed_liquidity` 方法:调之,返回 Python 端的 `next_id` 计数器
    /// - Python 对象无该方法:no-op,直接返回 `next_id`(不消费 ID)
    ///
    /// 典型实现:Python `ImpactedMatchingEngine` 继承 `L1MatchingEngine`,自动
    /// 拥有 `seed_liquidity` 方法(通过 `L1MatchingEngine` Python 绑定暴露)。
    /// 用户自定义撮合类如未实现该方法,BacktestEngine 跳过 seed,等价于
    /// `L1MatchingEngine` 路径(无对手盘)。
    fn seed_liquidity(
        &mut self,
        mid_price: f64,
        half_spread: f64,
        depth_levels: usize,
        size_per_level: f64,
        symbol: Symbol,
        next_id: u64,
    ) -> u64 {
        Python::attach(|py| {
            let engine = match self.inner.lock() {
                Ok(g) => g,
                Err(_) => return next_id,
            };
            let py_engine = engine.bind(py);
            // ponytail:Python 端缺 seed_liquidity 方法时降级为 no-op,
            // 不抛错(用户可能用 L2/L3 等无种子语义的撮合引擎)。
            let method = match py_engine.getattr("seed_liquidity") {
                Ok(m) => m,
                Err(_) => return next_id,
            };
            match method.call1((
                mid_price,
                half_spread,
                depth_levels,
                size_per_level,
                symbol.as_str(),
                next_id,
            )) {
                Ok(v) => v.extract::<u64>().unwrap_or(next_id),
                Err(e) => {
                    e.print(py);
                    next_id
                }
            }
        })
    }
}

impl PyMatchingEngine {
    /// 实际 submit 逻辑(返回 Result 供调用方处理 Python 异常)
    fn try_submit(&self, order: &Order) -> PyResult<SubmitResult> {
        Python::attach(|py| {
            let engine = self
                .inner
                .lock()
                .map_err(|_| pyo3::exceptions::PyRuntimeError::new_err("mutex poisoned"))?;
            let py_engine = engine.bind(py);
            let order_dict = order_to_dict(py, order)?;
            let result_any = py_engine.call_method1("submit", (order_dict,))?;
            let result_dict = result_any.cast::<PyDict>()?;
            submit_result_from_dict(result_dict)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_to_dict_contains_required_fields() {
        Python::attach(|py| {
            let order = Order::spot(
                42,
                "BTCUSDT",
                "USDT",
                Side::Buy,
                OrderType::Limit {
                    price: Price::from_f64(100.0),
                },
                Quantity::from_f64(0.001),
                TimeInForce::GTC,
            );
            let d = order_to_dict(py, &order).unwrap();
            assert_eq!(
                d.get_item("id").unwrap().unwrap().extract::<u64>().unwrap(),
                42
            );
            assert_eq!(
                d.get_item("symbol")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "BTCUSDT"
            );
            assert_eq!(
                d.get_item("side")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "buy"
            );
            assert_eq!(
                d.get_item("type")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "limit"
            );
            assert_eq!(
                d.get_item("price")
                    .unwrap()
                    .unwrap()
                    .extract::<f64>()
                    .unwrap(),
                100.0
            );
        });
    }
}
