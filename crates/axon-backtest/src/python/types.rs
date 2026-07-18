//! `axon-backtest` Python 绑定的共用 dict 协议 + 工具函数。
//!
//! 目标:把 `impact/python.rs` 中已有的 `dict_to_order` / `parse_side` /
//! `parse_tif` / `submit_result_to_dict` / `match_fill_to_dict` 抽到此处,
//! 让 L1/L2/L3 撮合引擎、`BacktestEngine` 都能复用同一组 dict 协议,
//! 避免每个子模块重复实现。
//!
//! # 数据契约
//!
//! Python ↔ Rust 的"主语"是 **Python**,所以:
//! - `Order` / `MatchFill` / `SubmitResult` 不直接暴露为 `#[pyclass]`,
//!   而是用 `dict` 协议:`{id, instrument, side, type, price, quantity, tif, ...}`。
//! - 这样可以避免在 Python 端需要 `axon_quant.backtest.Order()` 之类的
//!   包装,直接用 `dict` 即可。
//!
//! # 设计动机
//!
//! - 与 `impact/python.rs` 的 `PyImpactedMatchingEngine.submit(dict) -> dict` 模式保持一致。
//! - Stage 2 的 thin wrapper(在 `python/axon_quant/backtest.py`)可继续用 dict,
//!   不会产生 `pyclass` 抽象层。
//!
//! # 错误处理
//!
//! - `dict_to_order` 缺字段时返回 `PyKeyError`,类型不匹配返回 `PyValueError`。
//! - 解析 `side` / `type` / `tif` 字符串失败时返回 `PyValueError`,
//!   携带失败字段名便于 Python 端排查。

use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use axon_core::dict_field;
use axon_core::market::Side as CoreSide;
use axon_core::order::{Order, OrderType, TimeInForce};
use axon_core::parse_py_enum;
use axon_core::types::{
    Instrument, Price, Quantity, SpotInstrument, SwapInstrument, SwapSettle, Symbol,
};

use crate::matching::types::{MatchFill, SubmitResult};

// ─── Instrument 解析 ────────────────────────────────────

/// Python dict → Rust [`Instrument`]
///
/// 支持的 wire 格式(flat 形式,Python 友好):
/// - spot: `{"kind": "spot", "base": "BTC", "quote": "USDT"}`
/// - swap: `{"kind": "swap", "base": "BTC", "quote": "USDT",
///           "settle": "usd_margin" | "coin_margin",
///           "contract_size": 1.0}`
///
/// 字段大小写:`kind` / `settle` 不敏感;`base` / `quote` / `contract_size` 严格。
///
/// 错误:
/// - 缺 `kind` / `base` / `quote` → `PyKeyError`
/// - `kind` 值非法 / `settle` 值非法 → `PyValueError`
pub fn parse_instrument<'py>(dict: &Bound<'py, PyDict>) -> PyResult<Instrument> {
    let kind: String = dict_field!(dict, "kind", String);
    match kind.to_lowercase().as_str() {
        "spot" => {
            let base: String = dict_field!(dict, "base", String);
            let quote: String = dict_field!(dict, "quote", String);
            Ok(Instrument::Spot(SpotInstrument {
                base: Symbol::from(base),
                quote: Symbol::from(quote),
            }))
        }
        "swap" => {
            let base: String = dict_field!(dict, "base", String);
            let quote: String = dict_field!(dict, "quote", String);
            let settle: String = dict_field!(dict, "settle", String);
            let contract_size: f64 = dict_field!(dict, "contract_size", f64);
            let settle_enum = match settle.to_lowercase().as_str() {
                "usd_margin" => SwapSettle::UsdMargin,
                "coin_margin" => SwapSettle::CoinMargin,
                other => {
                    return Err(PyValueError::new_err(format!(
                        "invalid settle: {other} (expected 'usd_margin' / 'coin_margin')"
                    )));
                }
            };
            Ok(Instrument::Swap(SwapInstrument {
                base: Symbol::from(base),
                quote: Symbol::from(quote),
                settle: settle_enum,
                contract_size,
            }))
        }
        other => Err(PyValueError::new_err(format!(
            "invalid instrument kind: {other} (expected 'spot' / 'swap')"
        ))),
    }
}

// ─── Order 解析 ───────────────────────────────────────────

/// Python dict → Rust [`Order`]
///
/// 必填字段:
/// - `id` (`int`):订单 ID
/// - `instrument` (`dict`):交易品种,由 [`parse_instrument`] 解析
/// - `side` (`str`):`"buy"` / `"sell"`(大小写不敏感)
/// - `type` (`str`):`"market"` / `"limit"`(仅这两种,`"stop"` 等留给 L2)
/// - `quantity` (`float`):订单总数量
/// - `tif` (`str`):`"GTC"` / `"IOC"` / `"FOK"`(大小写不敏感)
///
/// 可选字段:
/// - `price` (`float`):限价单必填,市价单忽略
///
/// 错误:
/// - 缺字段 → `PyKeyError("missing '<field>'")`
/// - 字段类型不匹配 / 枚举值非法 → `PyValueError`
pub fn dict_to_order<'py>(dict: &Bound<'py, PyDict>) -> PyResult<Order> {
    let id: u64 = dict_field!(dict, "id", u64);
    let instrument_obj: Bound<'py, PyDict> = {
        let v = dict
            .get_item("instrument")?
            .ok_or_else(|| PyKeyError::new_err("missing 'instrument'"))?;
        v.extract::<Bound<'py, PyDict>>()
            .map_err(|_| PyValueError::new_err("field 'instrument' must be a dict"))?
    };
    let instrument = parse_instrument(&instrument_obj)?;
    let side_str: String = dict_field!(dict, "side", String);
    let side = parse_side(&side_str)?;
    let order_type_str: String = dict_field!(dict, "type", String);
    let quantity: f64 = dict_field!(dict, "quantity", f64);
    let tif_str: String = dict_field!(dict, "tif", String);
    let tif = parse_tif(&tif_str)?;

    let order_type = match order_type_str.to_lowercase().as_str() {
        "limit" => {
            let price: f64 = dict_field!(dict, "price", f64);
            OrderType::Limit {
                price: Price::from_f64(price),
            }
        }
        "market" => OrderType::Market,
        other => {
            return Err(PyValueError::new_err(format!(
                "unsupported order type: {other} (only 'market' / 'limit')"
            )));
        }
    };

    // 按 instrument 变体选构造器:spot → Order::spot;swap → Order::swap。
    // 同一个 instrument 一次解析,后续 handle_submit 路由会用同一份。
    let order = match instrument {
        Instrument::Spot(s) => Order::spot(
            id,
            s.base,
            s.quote,
            side,
            order_type,
            Quantity::from_f64(quantity),
            tif,
        ),
        Instrument::Swap(s) => Order::swap(
            id,
            s.base,
            s.quote,
            s.settle,
            s.contract_size,
            side,
            order_type,
            Quantity::from_f64(quantity),
            tif,
        ),
    };
    Ok(order)
}

// ─── 枚举解析 ────────────────────────────────────────────

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

// ─── 序列化辅助 ──────────────────────────────────────────

/// [`MatchFill`] → Python dict
///
/// 字段:`fill_id` / `taker_order_id` / `maker_order_id` / `price` /
/// `quantity` / `taker_side`(`"Buy"` / `"Sell"` 字符串,便于 JSON 序列化)/
/// `timestamp_ns`(纳秒时间戳,供应用层按时序配对开/平仓、计算夏普比率等)
pub fn match_fill_to_dict<'py>(py: Python<'py>, fill: &MatchFill) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("fill_id", fill.fill_id)?;
    dict.set_item("taker_order_id", fill.taker_order_id)?;
    dict.set_item("maker_order_id", fill.maker_order_id)?;
    dict.set_item("price", fill.price.as_f64())?;
    dict.set_item("quantity", fill.quantity.as_f64())?;
    // 使用 `Debug` 格式 (`"Buy"` / `"Sell"`) 而非 `Display` (`"BUY"` / `"SELL"`),
    // 与 Stage 1 `axon_data` 的 JSON 风格一致(全大写 enum tag)
    // —— 实际上保持 Display 的全大写形式,便于 Python 端 `assert d["taker_side"] == "BUY"`。
    dict.set_item("taker_side", format!("{}", fill.taker_side))?;
    // 纳秒时间戳:应用层据此做开/平仓配对、净值曲线采样、夏普计算
    dict.set_item("timestamp_ns", fill.timestamp.nanos)?;
    Ok(dict)
}

/// [`SubmitResult`] → Python dict
///
/// 字段:
/// - `fills` (`list[dict]`):本订单产生的所有成交
/// - `is_filled` (`bool`):是否已全部成交
/// - `is_partially_filled` (`bool`):是否部分成交
/// - `remaining_quantity` (`float`):剩余未成交量
pub fn submit_result_to_dict<'py>(
    py: Python<'py>,
    result: &SubmitResult,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);

    let fills_list = PyList::empty(py);
    for fill in &result.fills {
        fills_list.append(match_fill_to_dict(py, fill)?)?;
    }
    dict.set_item("fills", fills_list)?;
    dict.set_item("is_filled", result.is_filled)?;
    dict.set_item("is_partially_filled", result.is_partially_filled)?;
    dict.set_item("remaining_quantity", result.remaining_quantity.as_f64())?;
    Ok(dict)
}

// ─── 内部辅助 ────────────────────────────────────────────

/// 从 dict 中取必填字段,缺字段返回 `PyKeyError("missing '<field>'")`,
/// 当前模块无 `#[pyclass]`,只暴露辅助函数 + 公共 `register` 占位
/// (保持与 `error.rs` / `matching_l1.rs` 风格一致,便于 `mod.rs` 一行调用)。
///
/// 这里 `register` 是个 no-op:`types` 模块只有工具函数,没有 class 需要注册。
pub fn register(_parent: &Bound<'_, PyModule>) -> PyResult<()> {
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::exceptions::PyKeyError;

    /// 构造 spot instrument dict
    fn make_spot_dict<'py>(
        py: Python<'py>,
        base: &str,
        quote: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("kind", "spot")?;
        d.set_item("base", base)?;
        d.set_item("quote", quote)?;
        Ok(d)
    }

    /// `dict_to_order` 全部字段合法时返回正确 `Order`(spot)
    #[test]
    fn dict_to_order_full_fields_spot() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("id", 42u64).unwrap();
            d.set_item("instrument", make_spot_dict(py, "BTC", "USDT").unwrap())
                .unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "limit").unwrap();
            d.set_item("price", 100.0_f64).unwrap();
            d.set_item("quantity", 1.0_f64).unwrap();
            d.set_item("tif", "GTC").unwrap();
            let order = dict_to_order(&d).unwrap();
            assert_eq!(order.id, 42);
            assert!(matches!(order.instrument, Instrument::Spot(_)));
            assert_eq!(order.side, CoreSide::Buy);
            assert!(matches!(order.order_type, OrderType::Limit { .. }));
            assert_eq!(order.time_in_force, TimeInForce::GTC);
        });
    }

    /// `dict_to_order` 全部字段合法时返回正确 `Order`(swap)
    #[test]
    fn dict_to_order_full_fields_swap() {
        Python::attach(|py| {
            let inst = PyDict::new(py);
            inst.set_item("kind", "swap").unwrap();
            inst.set_item("base", "ETH").unwrap();
            inst.set_item("quote", "USDT").unwrap();
            inst.set_item("settle", "usd_margin").unwrap();
            inst.set_item("contract_size", 0.01_f64).unwrap();

            let d = PyDict::new(py);
            d.set_item("id", 7u64).unwrap();
            d.set_item("instrument", inst).unwrap();
            d.set_item("side", "sell").unwrap();
            d.set_item("type", "market").unwrap();
            d.set_item("quantity", 10.0_f64).unwrap();
            d.set_item("tif", "IOC").unwrap();
            let order = dict_to_order(&d).unwrap();
            assert!(matches!(order.instrument, Instrument::Swap(_)));
            if let Instrument::Swap(s) = &order.instrument {
                assert_eq!(s.contract_size, 0.01);
                assert_eq!(s.settle, SwapSettle::UsdMargin);
                assert_eq!(s.base.as_str(), "ETH");
            }
        });
    }

    /// `dict_to_order` 缺 `instrument` 字段时返回 `PyKeyError`
    #[test]
    fn dict_to_order_missing_instrument_raises() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("id", 1u64).unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "market").unwrap();
            d.set_item("quantity", 1.0_f64).unwrap();
            d.set_item("tif", "GTC").unwrap();
            let err = dict_to_order(&d).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
            let msg = err.to_string();
            assert!(
                msg.contains("instrument"),
                "expected 'instrument' in error, got: {msg}"
            );
        });
    }

    /// `dict_to_order` 缺字段时返回 `PyKeyError`,message 包含字段名
    #[test]
    fn dict_to_order_missing_field() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            // 故意少填 `tif`
            d.set_item("id", 1u64).unwrap();
            d.set_item("instrument", make_spot_dict(py, "BTC", "USDT").unwrap())
                .unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "market").unwrap();
            d.set_item("quantity", 1.0_f64).unwrap();
            let err = dict_to_order(&d).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
            let msg = err.to_string();
            assert!(msg.contains("tif"), "expected 'tif' in error, got: {msg}");
        });
    }

    /// `dict_to_order` 收到未知 `type` 时返回 `PyValueError`
    #[test]
    fn dict_to_order_invalid_type() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("id", 1u64).unwrap();
            d.set_item("instrument", make_spot_dict(py, "BTC", "USDT").unwrap())
                .unwrap();
            d.set_item("side", "buy").unwrap();
            d.set_item("type", "stop").unwrap(); // Stage 2 不支持 stop
            d.set_item("quantity", 1.0_f64).unwrap();
            d.set_item("tif", "GTC").unwrap();
            let err = dict_to_order(&d).unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    /// `parse_instrument` spot 路径
    #[test]
    fn parse_instrument_spot() {
        Python::attach(|py| {
            let d = make_spot_dict(py, "BTC", "USDT").unwrap();
            let inst = parse_instrument(&d).unwrap();
            assert!(matches!(inst, Instrument::Spot(_)));
            assert_eq!(inst.base().as_str(), "BTC");
            assert_eq!(inst.quote().as_str(), "USDT");
        });
    }

    /// `parse_instrument` swap 路径
    #[test]
    fn parse_instrument_swap() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("kind", "swap").unwrap();
            d.set_item("base", "BTC").unwrap();
            d.set_item("quote", "USDT").unwrap();
            d.set_item("settle", "coin_margin").unwrap();
            d.set_item("contract_size", 1.0_f64).unwrap();
            let inst = parse_instrument(&d).unwrap();
            assert!(matches!(inst, Instrument::Swap(_)));
            if let Instrument::Swap(s) = &inst {
                assert_eq!(s.settle, SwapSettle::CoinMargin);
                assert_eq!(s.contract_size, 1.0);
            }
        });
    }

    /// `parse_instrument` 非法 kind → `PyValueError`
    #[test]
    fn parse_instrument_invalid_kind() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("kind", "future").unwrap();
            d.set_item("base", "BTC").unwrap();
            d.set_item("quote", "USDT").unwrap();
            let err = parse_instrument(&d).unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    /// `parse_instrument` swap 缺 `settle` → `PyKeyError`
    #[test]
    fn parse_instrument_swap_missing_settle() {
        Python::attach(|py| {
            let d = PyDict::new(py);
            d.set_item("kind", "swap").unwrap();
            d.set_item("base", "BTC").unwrap();
            d.set_item("quote", "USDT").unwrap();
            d.set_item("contract_size", 1.0_f64).unwrap();
            let err = parse_instrument(&d).unwrap_err();
            assert!(err.is_instance_of::<PyKeyError>(py));
        });
    }

    /// `parse_side` 大小写不敏感
    #[test]
    fn parse_side_case_insensitive() {
        assert!(matches!(parse_side("buy").unwrap(), CoreSide::Buy));
        assert!(matches!(parse_side("BUY").unwrap(), CoreSide::Buy));
        assert!(matches!(parse_side("Sell").unwrap(), CoreSide::Sell));
    }

    /// `parse_side` 非法值返回 `PyValueError`
    #[test]
    fn parse_side_invalid() {
        Python::attach(|py| {
            let err = parse_side("xxx").unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    /// `parse_tif` 支持 5 种有效期
    #[test]
    fn parse_tif_all_variants() {
        assert!(matches!(parse_tif("GTC").unwrap(), TimeInForce::GTC));
        assert!(matches!(parse_tif("ioc").unwrap(), TimeInForce::IOC));
        assert!(matches!(parse_tif("Fok").unwrap(), TimeInForce::FOK));
        assert!(matches!(parse_tif("GFD").unwrap(), TimeInForce::GFD));
        assert!(matches!(parse_tif("fak").unwrap(), TimeInForce::FAK));
    }

    /// `parse_tif` 非法值返回 `PyValueError`
    #[test]
    fn parse_tif_invalid() {
        Python::attach(|py| {
            let err = parse_tif("XXX").unwrap_err();
            assert!(err.is_instance_of::<PyValueError>(py));
        });
    }

    /// `match_fill_to_dict` 字段全在,值正确
    #[test]
    fn match_fill_to_dict_fields() {
        Python::attach(|py| {
            let fill = MatchFill {
                fill_id: 7,
                taker_order_id: 1,
                maker_order_id: 2,
                price: Price::from_f64(100.5),
                quantity: Quantity::from_f64(2.0),
                taker_side: CoreSide::Buy,
                timestamp: axon_core::time::Timestamp::from_nanos(1_000),
            };
            let d = match_fill_to_dict(py, &fill).unwrap();
            assert_eq!(
                d.get_item("fill_id")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                7
            );
            assert_eq!(
                d.get_item("taker_order_id")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                1
            );
            assert_eq!(
                d.get_item("maker_order_id")
                    .unwrap()
                    .unwrap()
                    .extract::<u64>()
                    .unwrap(),
                2
            );
            assert!(
                (d.get_item("price")
                    .unwrap()
                    .unwrap()
                    .extract::<f64>()
                    .unwrap()
                    - 100.5)
                    .abs()
                    < 1e-9
            );
            assert!(
                (d.get_item("quantity")
                    .unwrap()
                    .unwrap()
                    .extract::<f64>()
                    .unwrap()
                    - 2.0)
                    .abs()
                    < 1e-9
            );
            assert_eq!(
                d.get_item("taker_side")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "BUY"
            );
        });
    }

    /// `submit_result_to_dict` 包含 `fills` / `is_filled` / `is_partially_filled` / `remaining_quantity`
    #[test]
    fn submit_result_to_dict_fields() {
        Python::attach(|py| {
            let result = SubmitResult::filled(vec![MatchFill {
                fill_id: 1,
                taker_order_id: 1,
                maker_order_id: 2,
                price: Price::from_f64(100.0),
                quantity: Quantity::from_f64(1.0),
                taker_side: CoreSide::Sell,
                timestamp: axon_core::time::Timestamp::from_nanos(0),
            }]);
            let d = submit_result_to_dict(py, &result).unwrap();
            assert!(
                d.get_item("is_filled")
                    .unwrap()
                    .unwrap()
                    .extract::<bool>()
                    .unwrap()
            );
            assert!(
                !d.get_item("is_partially_filled")
                    .unwrap()
                    .unwrap()
                    .extract::<bool>()
                    .unwrap()
            );
            // fills 列表
            let fills = d.get_item("fills").unwrap().unwrap();
            assert_eq!(fills.len().unwrap(), 1);
        });
    }

    /// `register` 签名稳定(编译期断言)
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
