//! Python 端 `Portfolio` + `Position` —— 组合 + 持仓查询。
//!
//! ## 与 Rust API 的关键差异
//!
//! - Rust `Portfolio` 字段是 `pub cash: HashMap<String, Decimal>` +
//!   `pub positions: HashMap<String, Position>`,PyO3 不能把 `HashMap` 直接
//!   当 `#[pyclass]` 字段(没有 `IntoPy<HashMap<...>>`),需要 `#[getter]`
//!   方法手动转 `dict`。
//!
//! - Rust `Position` 字段是 `symbol` / `quantity` / `avg_price` /
//!   `realized_pnl` / `updated_at`(注意是 **realized_pnl**,不是
//!   `unrealized_pnl` —— unrealized 需要 mark-to-market 当前价,Stage 4
//!   不做)。Plan 5 v1.0 草稿写的 `unrealized_pnl` 与 Rust 不符,已按
//!   Rust 实际字段修正。
//!
//! - Rust `Portfolio` 没有 `update_position` / `get_position` /
//!   `total_value` 等方法(Plan 5 v1.0 草稿假设错误),只有:
//!   - `new()` / `default()`
//!   - `deposit(currency, amount)` —— 加现金
//!   - `apply_fill(fill: &Fill)` —— 消费一个成交事件,更新 cash + positions
//!   - `snapshot()` / `recover(snap)` / `from_snapshot(snap)` —— 持久化
//!
//!   Python 端**只**暴露 `new` / `deposit` / `apply_fill` 三个写方法,数据
//!   读取走 `cash` / `positions` getter 转 dict。
//!
//! - `apply_fill` 在 Rust 端签名是 `apply_fill(&mut self, fill: &Fill)`,
//!   `Fill` 是 5 字段结构(`fill_id` / `symbol` / `price` / `quantity` /
//!   `fee` / `timestamp`)。Python 端签名简化为:
//!   ```text
//!   apply_fill(fill_id, symbol, price, quantity, fee, timestamp=None)
//!   ```
//!   `timestamp=None` 时用 `Utc::now()`,符合 `Fill::default` 行为。
//!
//! - **生产推荐**:Python 端 `PyPortfolio` 主要供**只读访问**(通过
//!   `OrderManager.snapshot_balance` / `snapshot_positions` 桥接,见
//!   `manager.rs` 的 `snapshot_balance` / `snapshot_positions` 方法)。
//!   独立 `PyPortfolio` 实例适合单元测试或**手动离线对账**(如 replay
//!   fill 序列)场景。生产 OMS 流程请走 `OrderManager.add_fill` 触发
//!   portfolio 状态更新,避免绕开状态机导致 cash/positions 不一致。
//!
//! - **quote currency 硬编码 "USDT"**:这是 Rust 端的设计决策(见
//!   `portfolio.rs:12` 注释),Stage B-MVP+ 才会改可配。

use std::str::FromStr;

use chrono::{DateTime, Utc};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use rust_decimal::Decimal;

use crate::portfolio::{Portfolio as RustPortfolio, Position as RustPosition};
use crate::types::Fill as RustFill;

use super::decimal::{decimal_to_py, py_to_decimal};

// ─── Position ───────────────────────────────────────────

/// Python 端 `Position` —— 单个 symbol 的持仓状态
///
/// 字段全部只读,数值用 `str` repr(与 Order 一致,精度无损)。
#[pyclass(name = "Position", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyPosition {
    pub(crate) inner: RustPosition,
}

#[pymethods]
impl PyPosition {
    #[getter]
    fn symbol(&self) -> String {
        self.inner.symbol.clone()
    }
    #[getter]
    fn quantity(&self) -> String {
        // 净持仓:正=多,负=空。用 Decimal 字符串原样输出
        self.inner.quantity.to_string()
    }
    #[getter]
    fn avg_price(&self) -> String {
        // 加权平均成本(空头为开仓均价)
        self.inner.avg_price.to_string()
    }
    #[getter]
    fn realized_pnl(&self) -> String {
        // 实现盈亏累计(平仓时累计,未实现盈亏需 mark-to-market)
        self.inner.realized_pnl.to_string()
    }
    #[getter]
    fn updated_at(&self) -> String {
        // 转 ISO 8601 字符串,便于 Python datetime 解析
        self.inner.updated_at.to_rfc3339()
    }

    /// 序列化为 Python `dict`
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("symbol", &self.inner.symbol)?;
        d.set_item("quantity", self.inner.quantity.to_string())?;
        d.set_item("avg_price", self.inner.avg_price.to_string())?;
        d.set_item("realized_pnl", self.inner.realized_pnl.to_string())?;
        d.set_item("updated_at", self.inner.updated_at.to_rfc3339())?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "Position(symbol={}, quantity={}, avg_price={}, realized_pnl={})",
            self.inner.symbol, self.inner.quantity, self.inner.avg_price, self.inner.realized_pnl,
        )
    }
}

// ─── Portfolio ──────────────────────────────────────────

/// Python 端 `Portfolio` —— 多币种现金 + 多 symbol 持仓
///
/// **设计选择**:`inner` 用 `parking_lot::RwLock` 保护(Rust 端 `Portfolio`
/// 本身不带锁,这里加锁是因为 Python 端 GIL 释放后 Rust 内部可能仍被
/// 共享引用;实际生产用 `OrderManager.snapshot_balance` 走更稳健的
/// 桥接路径)。
#[pyclass(name = "Portfolio")]
pub struct PyPortfolio {
    inner: parking_lot::Mutex<RustPortfolio>,
}

#[pymethods]
impl PyPortfolio {
    #[new]
    fn new() -> Self {
        Self {
            inner: parking_lot::Mutex::new(RustPortfolio::new()),
        }
    }

    /// 加现金(增量累加,同 currency 累加而非覆盖)
    fn deposit(&self, currency: &str, amount: &Bound<'_, pyo3::types::PyAny>) -> PyResult<()> {
        let amt = py_to_decimal(amount)?;
        self.inner.lock().deposit(currency, amt);
        Ok(())
    }

    /// 取出现金(出金),余额不足时抛 ValueError
    fn withdraw(&self, currency: &str, amount: &Bound<'_, pyo3::types::PyAny>) -> PyResult<()> {
        let amt = py_to_decimal(amount)?;
        self.inner
            .lock()
            .withdraw(currency, amt)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        Ok(())
    }

    /// 应用一个 fill 事件
    ///
    /// **签名与 Rust 端对应**:`Fill` 字段是 `fill_id` / `symbol` / `price` /
    /// `quantity` / `fee` / `timestamp`。`quantity` 正=buy,负=sell
    /// (与 Rust 端 `apply_fill` 行为一致)。
    ///
    /// `timestamp=None` 时用 `Utc::now()`。
    #[pyo3(signature = (fill_id, symbol, price, quantity, fee, timestamp=None))]
    fn apply_fill(
        &self,
        fill_id: String,
        symbol: String,
        price: &Bound<'_, pyo3::types::PyAny>,
        quantity: &Bound<'_, pyo3::types::PyAny>,
        fee: &Bound<'_, pyo3::types::PyAny>,
        timestamp: Option<String>,
    ) -> PyResult<()> {
        let price = py_to_decimal(price)?;
        let quantity = py_to_decimal(quantity)?;
        let fee = py_to_decimal(fee)?;
        let ts = match timestamp {
            Some(s) => DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    pyo3::exceptions::PyValueError::new_err(format!(
                        "invalid RFC3339 timestamp: {e}"
                    ))
                })?,
            None => Utc::now(),
        };
        let fill = RustFill {
            fill_id,
            symbol,
            // 0.6.0 新增:Python 端 `apply_fill` 路径暂未携带结构化 instrument,
            // 留 None 让老路径继续走字符串 `symbol` 兜底
            instrument: None,
            price,
            quantity,
            fee,
            timestamp: ts,
        };
        self.inner
            .lock()
            .apply_fill(&fill)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        Ok(())
    }

    /// 现金余额字典:币种 → 数量(Decimal str)
    #[getter]
    fn cash<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (k, v) in self.inner.lock().cash.iter() {
            d.set_item(k, v.to_string())?;
        }
        Ok(d)
    }

    /// 持仓字典:symbol → `PyPosition`
    #[getter]
    fn positions<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (sym, pos) in self.inner.lock().positions.iter() {
            d.set_item(sym, PyPosition { inner: pos.clone() })?;
        }
        Ok(d)
    }

    /// 持仓数
    fn position_count(&self) -> usize {
        self.inner.lock().positions.len()
    }

    /// 组合是否完全空(无现金无持仓)
    fn is_empty(&self) -> bool {
        let p = self.inner.lock();
        p.cash.is_empty() && p.positions.is_empty()
    }

    /// 序列化为 Python `dict`(`cash` + `positions` + `position_count`)
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("cash", self.cash(py)?)?;
        d.set_item("positions", self.positions(py)?)?;
        d.set_item("position_count", self.position_count())?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        let p = self.inner.lock();
        format!(
            "Portfolio(currencies={}, positions={})",
            p.cash.len(),
            p.positions.len(),
        )
    }
}

// ─── helper(供 manager.rs 桥接) ─────────────────────────

/// 把 Rust `Position` 包装成 `PyPosition`(供 manager.rs 的
/// `snapshot_positions` 方法使用)。
pub fn wrap_position(pos: RustPosition) -> PyPosition {
    PyPosition { inner: pos }
}

/// 把 Rust `Portfolio` 序列化为 Python `dict`(供 manager.rs 的
/// `snapshot_balance` 方法使用,包含 `cash` + `positions` + `as_of`)。
///
/// **`as_of` 是 ISO 8601 字符串**;Python 端用 `datetime.fromisoformat`
/// 解析后即可与本地时区比较。
pub fn portfolio_to_dict<'py>(
    py: Python<'py>,
    snap: &crate::portfolio::PortfolioSnapshot,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    let cash_d = PyDict::new(py);
    for (k, v) in &snap.cash {
        cash_d.set_item(k, v.to_string())?;
    }
    d.set_item("cash", cash_d)?;
    let pos_d = PyDict::new(py);
    for p in &snap.positions {
        pos_d.set_item(&p.symbol, wrap_position(p.clone()))?;
    }
    d.set_item("positions", pos_d)?;
    d.set_item("as_of", snap.as_of.to_rfc3339())?;
    Ok(d)
}

/// 把 Rust `Decimal` 转 Python `Decimal`(供 manager.rs 的
/// `total_value` 等聚合查询使用,目前未直接暴露但保留备用)。
#[allow(dead_code)]
pub fn py_decimal<'py>(py: Python<'py>, d: Decimal) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
    decimal_to_py(py, &d)
}

/// 解析 Python `Decimal` → Rust `Decimal`(供 manager.rs 接收 Python 端
/// 数量时使用,目前未直接暴露但保留备用)。
#[allow(dead_code)]
pub fn rust_decimal(obj: &Bound<'_, pyo3::types::PyAny>) -> PyResult<Decimal> {
    py_to_decimal(obj)
}

/// 解析 Python 端传入的 `timestamp` 字符串(ISO 8601 / RFC 3339)→ `DateTime<Utc>`。
///
/// 供 manager.rs 接收 Python 端 fill 事件时使用(目前未直接暴露但保留备用)。
#[allow(dead_code)]
pub fn parse_ts(s: &str) -> PyResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid timestamp: {e}")))
}

/// 解析十进制数字字符串(高精度场景,避免 `Decimal::from_str` 的精度问题)。
#[allow(dead_code)]
pub fn parse_decimal(s: &str) -> PyResult<Decimal> {
    Decimal::from_str(s)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid decimal: {e}")))
}

/// 供 manager.rs 调用的 `Bound<PyAny>` → `Decimal` helper。
///
/// **为什么用 `_helper` 后缀**:与 `super::decimal::py_to_decimal` 功能相同,
/// 但语义上显式标注是给 `portfolio` 子模块之外的调用方使用,便于 grep。
#[allow(dead_code)]
pub fn parse_decimal_helper(obj: &Bound<'_, pyo3::types::PyAny>) -> PyResult<Decimal> {
    py_to_decimal(obj)
}

/// 供 manager.rs 调用的 `&str` → `DateTime<Utc>` helper。
#[allow(dead_code)]
pub fn parse_ts_helper(s: &str) -> PyResult<DateTime<Utc>> {
    parse_ts(s)
}

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyPosition>()?;
    parent.add_class::<PyPortfolio>()?;
    Ok(())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// `Position` getter 数值与 Rust 字段一致
    #[test]
    fn position_getters_match_inner() {
        let pos = RustPosition {
            symbol: "BTC-USDT".into(),
            quantity: dec!(1.5),
            avg_price: dec!(50000),
            realized_pnl: dec!(250),
            updated_at: Utc::now(),
        };
        let py_pos = PyPosition { inner: pos.clone() };
        assert_eq!(py_pos.symbol(), "BTC-USDT");
        assert_eq!(py_pos.quantity(), "1.5");
        assert_eq!(py_pos.avg_price(), "50000");
        assert_eq!(py_pos.realized_pnl(), "250");
        // updated_at 是 ISO 8601 字符串(非空 + 包含 'T' 分隔符)
        let s = py_pos.updated_at();
        assert!(s.contains('T'), "RFC3339 should contain 'T', got: {s}");
    }

    /// `Position::to_dict` 包含所有字段
    #[test]
    fn position_to_dict_contains_all_fields() {
        Python::attach(|py| {
            let pos = PyPosition {
                inner: RustPosition {
                    symbol: "ETH-USDT".into(),
                    quantity: dec!(-2),
                    avg_price: dec!(3000),
                    realized_pnl: dec!(-50),
                    updated_at: Utc::now(),
                },
            };
            let d = pos.to_dict(py).unwrap();
            assert_eq!(
                d.get_item("symbol")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "ETH-USDT"
            );
            assert_eq!(
                d.get_item("quantity")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "-2"
            );
            assert_eq!(
                d.get_item("avg_price")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "3000"
            );
            assert_eq!(
                d.get_item("realized_pnl")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "-50"
            );
            assert!(d.get_item("updated_at").unwrap().is_some());
        });
    }

    /// `Portfolio::new()` 创建空组合
    #[test]
    fn portfolio_new_is_empty() {
        let p = PyPortfolio::new();
        assert!(p.is_empty());
        assert_eq!(p.position_count(), 0);
    }

    /// `deposit` 累加同一币种
    #[test]
    fn portfolio_deposit_accumulates() {
        Python::attach(|py| {
            let p = PyPortfolio::new();
            let decimal_mod = py.import("decimal").unwrap();
            let amt1 = decimal_mod.call_method1("Decimal", ("1000",)).unwrap();
            let amt2 = decimal_mod.call_method1("Decimal", ("500.5",)).unwrap();
            p.deposit("USDT", &amt1).unwrap();
            p.deposit("USDT", &amt2).unwrap();
            let cash = p.cash(py).unwrap();
            let usdt: String = cash.get_item("USDT").unwrap().unwrap().extract().unwrap();
            assert_eq!(usdt, "1500.5");
        });
    }

    /// `deposit` 接受浮点 + int(走 `py_to_decimal` 桥)
    #[test]
    fn portfolio_deposit_accepts_float() {
        Python::attach(|py| {
            let p = PyPortfolio::new();
            let py_obj = py.eval(c"123.45", None, None).unwrap();
            p.deposit("USDT", &py_obj).unwrap();
            let cash = p.cash(py).unwrap();
            let usdt: String = cash.get_item("USDT").unwrap().unwrap().extract().unwrap();
            assert_eq!(usdt, "123.45");
        });
    }

    /// `apply_fill` buy 建仓 + 扣现金
    #[test]
    fn portfolio_apply_fill_buy() {
        Python::attach(|py| {
            let p = PyPortfolio::new();
            let decimal_mod = py.import("decimal").unwrap();
            let deposit = decimal_mod.call_method1("Decimal", ("10000",)).unwrap();
            p.deposit("USDT", &deposit).unwrap();
            // buy 0.1 @ 50000 = 5000 USDT(扣)
            p.apply_fill(
                "f1".into(),
                "BTC-USDT".into(),
                &decimal_mod.call_method1("Decimal", ("50000",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("0.1",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("0",)).unwrap(),
                None,
            )
            .unwrap();
            // cash 10000 - 5000 = 5000(Python Decimal 保留 .0)
            let cash = p.cash(py).unwrap();
            let usdt: String = cash.get_item("USDT").unwrap().unwrap().extract().unwrap();
            assert_eq!(usdt, "5000.0", "expected '5000.0', got '{usdt}'");
            // 持仓建立
            let pos = p.get_position(py, "BTC-USDT").unwrap();
            assert_eq!(pos.symbol(), "BTC-USDT");
            assert_eq!(pos.quantity(), "0.1");
            assert_eq!(pos.avg_price(), "50000");
        });
    }

    /// `apply_fill` sell 部分平仓触发 realized_pnl
    #[test]
    fn portfolio_apply_fill_sell_realizes_pnl() {
        Python::attach(|py| {
            let p = PyPortfolio::new();
            let decimal_mod = py.import("decimal").unwrap();
            let deposit = decimal_mod.call_method1("Decimal", ("100000",)).unwrap();
            p.deposit("USDT", &deposit).unwrap();
            // buy 1 @ 50000
            p.apply_fill(
                "f1".into(),
                "BTC-USDT".into(),
                &decimal_mod.call_method1("Decimal", ("50000",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("1",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("0",)).unwrap(),
                None,
            )
            .unwrap();
            // sell 0.5 @ 55000 → realized = (55000 - 50000) * 0.5 = 2500
            p.apply_fill(
                "f2".into(),
                "BTC-USDT".into(),
                &decimal_mod.call_method1("Decimal", ("55000",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("-0.5",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("0",)).unwrap(),
                None,
            )
            .unwrap();
            let pos = p.get_position(py, "BTC-USDT").unwrap();
            assert_eq!(pos.quantity(), "0.5");
            // Python Decimal 计算 (55000 - 50000) * 0.5 = 2500.0
            assert_eq!(pos.realized_pnl(), "2500.0");
        });
    }

    /// `apply_fill` 现金不足时返回 `PyValueError`
    #[test]
    fn portfolio_apply_fill_insufficient_cash_raises() {
        Python::attach(|py| {
            let p = PyPortfolio::new();
            let decimal_mod = py.import("decimal").unwrap();
            // 只存 100,买 1 @ 50000
            p.deposit(
                "USDT",
                &decimal_mod.call_method1("Decimal", ("100",)).unwrap(),
            )
            .unwrap();
            let result = p.apply_fill(
                "f1".into(),
                "BTC-USDT".into(),
                &decimal_mod.call_method1("Decimal", ("50000",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("1",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("0",)).unwrap(),
                None,
            );
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(
                err.to_string().contains("insufficient cash"),
                "got: {}",
                err
            );
        });
    }

    /// `apply_fill` 接受 RFC 3339 timestamp 字符串
    #[test]
    fn portfolio_apply_fill_with_timestamp() {
        Python::attach(|py| {
            let p = PyPortfolio::new();
            let decimal_mod = py.import("decimal").unwrap();
            p.deposit(
                "USDT",
                &decimal_mod.call_method1("Decimal", ("100000",)).unwrap(),
            )
            .unwrap();
            let ts = "2026-01-15T10:00:00+00:00";
            p.apply_fill(
                "f1".into(),
                "BTC-USDT".into(),
                &decimal_mod.call_method1("Decimal", ("50000",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("1",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("0",)).unwrap(),
                Some(ts.into()),
            )
            .unwrap();
            let pos = p.get_position(py, "BTC-USDT").unwrap();
            assert!(pos.updated_at().contains("2026-01-15"));
        });
    }

    /// `apply_fill` 无效 timestamp → `PyValueError`
    #[test]
    fn portfolio_apply_fill_invalid_timestamp_raises() {
        Python::attach(|py| {
            let p = PyPortfolio::new();
            let decimal_mod = py.import("decimal").unwrap();
            p.deposit(
                "USDT",
                &decimal_mod.call_method1("Decimal", ("100000",)).unwrap(),
            )
            .unwrap();
            let result = p.apply_fill(
                "f1".into(),
                "BTC-USDT".into(),
                &decimal_mod.call_method1("Decimal", ("50000",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("1",)).unwrap(),
                &decimal_mod.call_method1("Decimal", ("0",)).unwrap(),
                Some("not-a-timestamp".into()),
            );
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("invalid"));
        });
    }

    /// `get_position` 不存在时返回 `None`
    #[test]
    fn portfolio_get_position_missing_returns_none() {
        Python::attach(|py| {
            let p = PyPortfolio::new();
            let result = p.get_position(py, "NONEXISTENT");
            assert!(result.is_none());
        });
    }

    /// `to_dict` 包含 cash + positions + position_count
    #[test]
    fn portfolio_to_dict_structure() {
        Python::attach(|py| {
            let p = PyPortfolio::new();
            let decimal_mod = py.import("decimal").unwrap();
            p.deposit(
                "USDT",
                &decimal_mod.call_method1("Decimal", ("1000",)).unwrap(),
            )
            .unwrap();
            let d = p.to_dict(py).unwrap();
            assert!(d.get_item("cash").unwrap().is_some());
            assert!(d.get_item("positions").unwrap().is_some());
            assert_eq!(
                d.get_item("position_count")
                    .unwrap()
                    .unwrap()
                    .extract::<usize>()
                    .unwrap(),
                0
            );
        });
    }

    /// `portfolio_to_dict` helper 转换 snapshot
    #[test]
    fn portfolio_to_dict_helper_converts_snapshot() {
        Python::attach(|py| {
            let mut p = RustPortfolio::new();
            p.deposit("USDT", dec!(5000));
            let snap = p.snapshot();
            let d = portfolio_to_dict(py, &snap).unwrap();
            // cash.USDT = 5000
            let cash_d: Bound<'_, PyDict> = d.get_item("cash").unwrap().unwrap().extract().unwrap();
            assert_eq!(
                cash_d
                    .get_item("USDT")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "5000"
            );
            // positions 为空
            assert!(
                d.get_item("position_count").unwrap().is_none(),
                "PortfolioSnapshot 不含 position_count 字段(由 PyPortfolio.to_dict 添加)"
            );
            // as_of 是 RFC3339
            let as_of: String = d.get_item("as_of").unwrap().unwrap().extract().unwrap();
            assert!(as_of.contains('T'));
        });
    }
}

// 额外方法(放在主 impl 之外作为 helper trait,便于测试使用)
impl PyPortfolio {
    /// 供测试访问单 symbol 持仓(避免暴露 `inner`)
    pub fn get_position(&self, _py: Python<'_>, symbol: &str) -> Option<PyPosition> {
        self.inner
            .lock()
            .positions
            .get(symbol)
            .cloned()
            .map(|inner| PyPosition { inner })
    }
}
