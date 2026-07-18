//! Python 端 `ComplianceModule` —— 统一合规审计入口。
//!
//! ## 与 Rust API 的关键差异
//!
//! - **构造方式**:Rust 端 `ComplianceModule::new(config, storage_path)` 接受
//!   `&ComplianceConfig` 和 `&Path` 两个参数;Python 端 `PyComplianceModule::new`
//!   接受**两类构造**:
//!   - `(config: PyComplianceConfig, storage_path: str)` —— 推荐,直接传 `ComplianceConfig` pyclass
//!   - `load_config_from_toml(path: str, storage_path: Option<str>)` —— 备选,
//!     兼容 Stage 1 风格的"从 TOML 读 config"
//!
//! - **`record_trade(dict)`**:用 dict 协议接收 trade(降门槛),内部调
//!   `python::types::parse_*` 把字符串 side / order_type / liquidity / status
//!   转为 Rust 枚举;UUID 字段缺省自动生成;DateTime 字段缺省用 `Utc::now()`。
//!
//! - **报告**:Rust 端 `generate_daily_report` / `_monthly_report` / `_annual_report`
//!   接受原生 `DailyReport` / `MonthlyReport` / `AnnualReport` struct,Python 端
//!   直接序列化为 Python dict(`serde_json` round-trip)返回,避免暴露 30+ 字段
//!   的 pyclass(同 `axon-risk` / `axon-oms` 模式)。
//!
//! - **`audit_event_count` getter**:Python 端用 `compliance.audit_event_count` 属性
//!   读审计日志长度,比调 `audit_log().len()` 直白。
//!
//! - **`trade_count` getter**:Python 端 `compliance.trade_count` 属性读交易数。
//!
//! - **同步包装**:合规报告生成、监管报送都是 CPU 同步计算,无 async 依赖,
//!   Python 端不需要 `block_on` 包装。

use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};
use pyo3::exceptions::{PyIOError, PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use uuid::Uuid;

use crate::ComplianceModule as RustModule;
use crate::types::{
    ComplianceConfig as RustConfig, TradeFilter as RustFilter, TradeRecord as RustTradeRecord,
};

use super::error::to_py_err;
use super::types::{
    PyComplianceConfig, parse_liquidity, parse_order_type, parse_side, parse_status,
};

#[allow(unused_imports)]
use pyo3::types::PyAnyMethods; // 用 `is_instance_of::<PyComplianceConfig>` 需要 `PyAnyMethods` trait

// ═══════════════════════════════════════════════════════════════════════════
// 主类: PyComplianceModule
// ═══════════════════════════════════════════════════════════════════════════

/// Python 端 `ComplianceModule` —— 合规审计统一入口。
///
/// 内部持有 `ComplianceModule`,所有 `&self` / `&mut self` 方法直接转发。
#[pyclass(name = "ComplianceModule", skip_from_py_object)]
pub struct PyComplianceModule {
    inner: Mutex<RustModule>,
    /// 冗余存的 storage path(给 `storage_path` getter / `__repr__` 用)
    storage_path: String,
}

#[pymethods]
impl PyComplianceModule {
    /// 构造合规模块(两种重载):
    ///
    /// **方式 1**(推荐):传 `ComplianceConfig` pyclass + storage_path
    /// ```python
    /// cfg = ComplianceConfig(
    ///     account_id="acc-1", base_currency="USDT",
    ///     large_trade_threshold=100_000.0, position_limit=1_000_000.0,
    ///     max_portfolio_concentration=0.4, data_retention_years=7,
    ///     regulators=["SEC"],
    /// )
    /// cm = ComplianceModule(cfg, "/tmp/compliance")
    /// ```
    ///
    /// **方式 2**:从 TOML 配置 + storage_path
    /// ```python
    /// cm = ComplianceModule("/path/to/config.toml")  # 旧风格,保持兼容
    /// ```
    #[new]
    fn new(config_or_path: &Bound<'_, PyAny>, storage_path: Option<&str>) -> PyResult<Self> {
        // 优先检查是否是 PyComplianceConfig 实例
        if config_or_path.is_instance_of::<PyComplianceConfig>() {
            // 方式 1:PyComplianceConfig + storage_path(必填)
            let path = storage_path.ok_or_else(|| {
                PyValueError::new_err("ComplianceModule(config, ...) requires `storage_path` arg")
            })?;
            let pathbuf = PathBuf::from(path);
            std::fs::create_dir_all(&pathbuf)
                .map_err(|e| PyIOError::new_err(format!("create_dir_all({path:?}) failed: {e}")))?;
            // 用 cfg() 方法拿内部 RustConfig(避免 extract::<T>() 的 Clone bound)
            // 0.6.0 起:用 safe 的 `cast::<T>()` 替代 `cast_unchecked::<T>()`。
            // 上面的 `is_instance_of::<PyComplianceConfig>()` 已经在类型层保证
            // cast 一定成功,这里用 `expect` 把"理论上不会失败"的不变量
            // 显式化(失败 = PyO3 invariant bug,panic 优于 UB)。
            let py_cfg = config_or_path
                .cast::<PyComplianceConfig>()
                .expect("is_instance_of::<PyComplianceConfig> checked above");
            let cfg = py_cfg.borrow().0.clone();
            let inner = RustModule::new(cfg, &pathbuf).map_err(to_py_err)?;
            return Ok(Self {
                inner: Mutex::new(inner),
                storage_path: path.to_string(),
            });
        }
        if let Ok(path_str) = config_or_path.extract::<String>() {
            // 方式 2:TOML config 路径(默认 storage 路径 = data/compliance/{account_id})
            let config_str = std::fs::read_to_string(&path_str).map_err(|e| {
                PyIOError::new_err(format!("read_to_string({path_str}) failed: {e}"))
            })?;
            let config: RustConfig = toml::from_str(&config_str)
                .map_err(|e| PyValueError::new_err(format!("parse TOML config failed: {e}")))?;
            let path_str_storage = storage_path
                .map(String::from)
                .unwrap_or_else(|| format!("data/compliance/{}", config.account_id));
            let pathbuf = PathBuf::from(&path_str_storage);
            std::fs::create_dir_all(&pathbuf).map_err(|e| {
                PyIOError::new_err(format!("create_dir_all({pathbuf:?}) failed: {e}"))
            })?;
            let inner = RustModule::new(config, &pathbuf).map_err(to_py_err)?;
            Ok(Self {
                inner: Mutex::new(inner),
                storage_path: path_str_storage,
            })
        } else {
            Err(PyValueError::new_err(
                "ComplianceModule expects (ComplianceConfig, storage_path) or (str_toml_path)",
            ))
        }
    }

    /// 记录单笔交易(dict 协议)。
    ///
    /// **必填字段**(`KeyError` if missing):
    /// - `strategy_id: str`
    /// - `symbol: str`
    /// - `side: str` ("buy" / "sell")
    /// - `quantity: float` (> 0)
    /// - `price: float` (> 0)
    /// - `fee: float`
    /// - `fee_currency: str`
    /// - `exchange: str`
    ///
    /// **可选字段**(缺省用合理默认):
    /// - `trade_id: str` (UUID 字符串,缺省自动生成)
    /// - `order_id: str` (UUID 字符串,缺省自动生成)
    /// - `execution_time: str` (RFC3339,缺省用当前 UTC)
    /// - `settlement_time: str` (RFC3339)
    /// - `status: str` (默认 "filled")
    /// - `order_type: str` (默认 "market")
    /// - `exchange_trade_id: str`
    /// - `liquidity: str` (默认 "taker")
    /// - `realized_pnl: float`
    /// - `funding_rate: float`
    /// - `slippage: float`
    ///
    /// 错误:
    /// - `KeyError`:缺必填字段
    /// - `ValueError`:字段类型错 / UUID 解析失败 / 状态字符串无效
    /// - `ComplianceError`:数量/价格 ≤ 0 / notional 不匹配 / 审计失败
    fn record_trade(&self, trade: &Bound<'_, PyDict>) -> PyResult<()> {
        // ─── 必填字段 ───
        let strategy_id: String = get_dict_string(trade, "strategy_id")?;
        let symbol: String = get_dict_string(trade, "symbol")?;
        let side_str: String = get_dict_string(trade, "side")?;
        let quantity: f64 = get_dict_f64(trade, "quantity")?;
        let price: f64 = get_dict_f64(trade, "price")?;
        let fee: f64 = get_dict_f64(trade, "fee")?;
        let fee_currency: String = get_dict_string(trade, "fee_currency")?;
        let exchange: String = get_dict_string(trade, "exchange")?;

        // ─── 可选字段 ───
        let trade_id: Uuid = match trade.get_item("trade_id")? {
            Some(v) => {
                let s: String = v.extract().map_err(|_e| {
                    PyValueError::new_err("field 'trade_id' must be a string (UUID)")
                })?;
                Uuid::parse_str(&s)
                    .map_err(|e| PyValueError::new_err(format!("invalid trade_id UUID: {e}")))?
            }
            None => Uuid::new_v4(),
        };
        let order_id: Uuid = match trade.get_item("order_id")? {
            Some(v) => {
                let s: String = v.extract().map_err(|_e| {
                    PyValueError::new_err("field 'order_id' must be a string (UUID)")
                })?;
                Uuid::parse_str(&s)
                    .map_err(|e| PyValueError::new_err(format!("invalid order_id UUID: {e}")))?
            }
            None => Uuid::new_v4(),
        };
        let execution_time: DateTime<Utc> = match trade.get_item("execution_time")? {
            Some(v) => {
                let s: String = v.extract().map_err(|_e| {
                    PyValueError::new_err("field 'execution_time' must be a string (RFC3339)")
                })?;
                DateTime::parse_from_rfc3339(&s)
                    .map_err(|e| {
                        PyValueError::new_err(format!("invalid execution_time RFC3339: {e}"))
                    })?
                    .with_timezone(&Utc)
            }
            None => Utc::now(),
        };
        let settlement_time: Option<DateTime<Utc>> = match trade.get_item("settlement_time")? {
            Some(v) => {
                let s: String = v.extract().map_err(|_e| {
                    PyValueError::new_err("field 'settlement_time' must be a string (RFC3339)")
                })?;
                Some(
                    DateTime::parse_from_rfc3339(&s)
                        .map_err(|e| {
                            PyValueError::new_err(format!("invalid settlement_time RFC3339: {e}"))
                        })?
                        .with_timezone(&Utc),
                )
            }
            None => None,
        };
        let status = parse_status(get_dict_optional_string(trade, "status")?.as_deref())?;
        let order_type =
            parse_order_type(get_dict_optional_string(trade, "order_type")?.as_deref())?;
        let side = parse_side(&side_str)?;
        let exchange_trade_id: Option<String> =
            get_dict_optional_string(trade, "exchange_trade_id")?;
        let liquidity = parse_liquidity(get_dict_optional_string(trade, "liquidity")?.as_deref())?;
        let realized_pnl: Option<f64> = get_dict_optional_f64(trade, "realized_pnl")?;
        let funding_rate: Option<f64> = get_dict_optional_f64(trade, "funding_rate")?;
        let slippage: Option<f64> = get_dict_optional_f64(trade, "slippage")?;
        let notional_value = quantity * price;

        let trade = RustTradeRecord {
            trade_id,
            order_id,
            strategy_id,
            symbol,
            side,
            quantity,
            price,
            notional_value,
            fee,
            fee_currency,
            exchange,
            execution_time,
            settlement_time,
            status,
            order_type,
            exchange_trade_id,
            liquidity,
            realized_pnl,
            funding_rate,
            slippage,
            created_at: Utc::now(),
        };

        let mut inner = self
            .inner
            .lock()
            .map_err(|_e| PyValueError::new_err("compliance module lock poisoned"))?;
        inner.record_trade(trade).map_err(to_py_err)?;
        Ok(())
    }

    /// 当前已记录的交易数
    #[getter]
    fn trade_count(&self) -> PyResult<usize> {
        let inner = self
            .inner
            .lock()
            .map_err(|_e| PyValueError::new_err("compliance module lock poisoned"))?;
        Ok(inner.trade_count())
    }

    /// 当前审计日志事件数
    #[getter]
    fn audit_event_count(&self) -> PyResult<usize> {
        let inner = self
            .inner
            .lock()
            .map_err(|_e| PyValueError::new_err("compliance module lock poisoned"))?;
        Ok(inner.audit_log().len())
    }

    /// 合规配置 getter
    #[getter]
    fn config(&self) -> PyResult<PyComplianceConfig> {
        let inner = self
            .inner
            .lock()
            .map_err(|_e| PyValueError::new_err("compliance module lock poisoned"))?;
        Ok(PyComplianceConfig(inner.config().clone()))
    }

    /// 存储路径 getter
    #[getter]
    fn storage_path(&self) -> String {
        self.storage_path.clone()
    }

    /// 验证审计日志完整性(区块链式哈希链校验)
    fn verify_audit_integrity(&self) -> PyResult<bool> {
        let inner = self
            .inner
            .lock()
            .map_err(|_e| PyValueError::new_err("compliance module lock poisoned"))?;
        Ok(inner.verify_audit_integrity())
    }

    /// 按过滤条件查询交易记录,返回 list[dict]。
    ///
    /// **过滤字段**(全部 optional):
    /// - `symbol: str`
    /// - `strategy_id: str`
    /// - `side: str` ("buy" / "sell")
    /// - `status: str` ("filled" / "cancelled" 等)
    /// - `min_notional: float`
    /// - `start_time: str` (RFC3339)
    /// - `end_time: str` (RFC3339)
    fn query_trades(&self, py: Python<'_>, filter: &Bound<'_, PyDict>) -> PyResult<Py<PyAny>> {
        let f = RustFilter {
            symbol: get_dict_optional_string(filter, "symbol")?,
            strategy_id: get_dict_optional_string(filter, "strategy_id")?,
            side: match get_dict_optional_string(filter, "side")? {
                Some(s) => Some(parse_side(&s)?),
                None => None,
            },
            status: match get_dict_optional_string(filter, "status")? {
                Some(s) => Some(parse_status(Some(&s))?),
                None => None,
            },
            start_time: get_dict_optional_string(filter, "start_time")?
                .map(|s| {
                    DateTime::parse_from_rfc3339(&s)
                        .map(|d| d.with_timezone(&Utc))
                        .map_err(|e| {
                            PyValueError::new_err(format!("invalid start_time RFC3339: {e}"))
                        })
                })
                .transpose()?,
            end_time: get_dict_optional_string(filter, "end_time")?
                .map(|s| {
                    DateTime::parse_from_rfc3339(&s)
                        .map(|d| d.with_timezone(&Utc))
                        .map_err(|e| {
                            PyValueError::new_err(format!("invalid end_time RFC3339: {e}"))
                        })
                })
                .transpose()?,
            min_notional: get_dict_optional_f64(filter, "min_notional")?,
        };
        let inner = self
            .inner
            .lock()
            .map_err(|_e| PyValueError::new_err("compliance module lock poisoned"))?;
        let trades = inner.query_trades(&f);
        // 序列化为 Python list[dict]
        let json = serde_json::to_string(&trades)
            .map_err(|e| PyValueError::new_err(format!("serialize trades failed: {e}")))?;
        let list: Py<PyAny> = py.import("json")?.call_method1("loads", (json,))?.unbind();
        Ok(list)
    }

    /// 拿交易统计 `TradeStats`(给定时间范围),返回 dict。
    ///
    /// 参数:`start_time: str` (RFC3339), `end_time: str` (RFC3339)
    fn get_trade_stats(
        &self,
        py: Python<'_>,
        start_time: &str,
        end_time: &str,
    ) -> PyResult<Py<PyAny>> {
        let start = DateTime::parse_from_rfc3339(start_time)
            .map_err(|e| PyValueError::new_err(format!("invalid start_time RFC3339: {e}")))?
            .with_timezone(&Utc);
        let end = DateTime::parse_from_rfc3339(end_time)
            .map_err(|e| PyValueError::new_err(format!("invalid end_time RFC3339: {e}")))?
            .with_timezone(&Utc);
        let inner = self
            .inner
            .lock()
            .map_err(|_e| PyValueError::new_err("compliance module lock poisoned"))?;
        let stats = inner.get_trade_stats(start, end);
        let json = serde_json::to_string(&stats)
            .map_err(|e| PyValueError::new_err(format!("serialize stats failed: {e}")))?;
        let dict: Py<PyAny> = py.import("json")?.call_method1("loads", (json,))?.unbind();
        Ok(dict)
    }

    /// 生成日报,返回 dict(同 Rust `DailyReport` 序列化)。
    ///
    /// 参数:`date: str` (ISO date "YYYY-MM-DD"), `starting_balance: float`
    fn generate_daily_report(
        &self,
        py: Python<'_>,
        date: &str,
        starting_balance: f64,
    ) -> PyResult<Py<PyAny>> {
        let d = NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|e| PyValueError::new_err(format!("invalid date (YYYY-MM-DD): {e}")))?;
        let inner = self
            .inner
            .lock()
            .map_err(|_e| PyValueError::new_err("compliance module lock poisoned"))?;
        let report = inner.generate_daily_report(d, starting_balance);
        report_to_dict(py, &report)
    }

    /// 生成月报,返回 dict。
    ///
    /// 参数:`year: int`, `month: int` (1-12)
    fn generate_monthly_report(
        &self,
        py: Python<'_>,
        year: u32,
        month: u32,
    ) -> PyResult<Py<PyAny>> {
        let inner = self
            .inner
            .lock()
            .map_err(|_e| PyValueError::new_err("compliance module lock poisoned"))?;
        let report = inner
            .generate_monthly_report(year, month)
            .map_err(to_py_err)?;
        report_to_dict(py, &report)
    }

    /// 生成年报,返回 dict。
    ///
    /// 参数:`year: int`, `initial_balance: float`
    fn generate_annual_report(
        &self,
        py: Python<'_>,
        year: u32,
        initial_balance: f64,
    ) -> PyResult<Py<PyAny>> {
        let inner = self
            .inner
            .lock()
            .map_err(|_e| PyValueError::new_err("compliance module lock poisoned"))?;
        let report = inner.generate_annual_report(year, initial_balance);
        report_to_dict(py, &report)
    }

    fn __repr__(&self) -> PyResult<String> {
        let cfg = self.config()?;
        Ok(format!(
            "ComplianceModule(account_id={:?}, base_currency={:?}, storage_path={:?})",
            cfg.0.account_id, cfg.0.base_currency, self.storage_path
        ))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 顶层工厂:从 TOML 配置一步创建
// ═══════════════════════════════════════════════════════════════════════════

/// 从 TOML 配置文件一步创建 `ComplianceModule`(旧风格保留)。
///
/// **参数**:
/// - `config_path: str` — TOML 配置文件路径
/// - `storage_path: Optional[str]` — 存储目录(缺省 = `data/compliance/{account_id}`)
///
/// **TOML 格式**:
/// ```toml
/// account_id = "acc-001"
/// base_currency = "USDT"
/// large_trade_threshold = 100000.0
/// position_limit = 1000000.0
/// max_portfolio_concentration = 0.4
/// data_retention_years = 7
/// regulators = ["SEC"]
/// ```
#[pyfunction]
#[pyo3(signature = (config_path, storage_path=None))]
pub fn load_config_from_toml(
    config_path: &str,
    storage_path: Option<&str>,
) -> PyResult<PyComplianceModule> {
    let config_str = std::fs::read_to_string(config_path)
        .map_err(|e| PyIOError::new_err(format!("read_to_string({config_path}) failed: {e}")))?;
    let config: RustConfig = toml::from_str(&config_str)
        .map_err(|e| PyValueError::new_err(format!("parse TOML config failed: {e}")))?;
    let path = storage_path
        .map(String::from)
        .unwrap_or_else(|| format!("data/compliance/{}", config.account_id));
    let pathbuf = PathBuf::from(&path);
    std::fs::create_dir_all(&pathbuf)
        .map_err(|e| PyIOError::new_err(format!("create_dir_all({pathbuf:?}) failed: {e}")))?;
    let inner = RustModule::new(config, &pathbuf).map_err(to_py_err)?;
    Ok(PyComplianceModule {
        inner: Mutex::new(inner),
        storage_path: path,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// 模块注册
// ═══════════════════════════════════════════════════════════════════════════

pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyComplianceModule>()?;
    parent.add_function(wrap_pyfunction!(load_config_from_toml, parent)?)?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// 内部辅助
// ═══════════════════════════════════════════════════════════════════════════

/// 从 Python dict 读取**必填**字符串字段,缺字段抛 `KeyError`。
fn get_dict_string(d: &Bound<'_, PyDict>, key: &str) -> PyResult<String> {
    d.get_item(key)?
        .ok_or_else(|| PyKeyError::new_err(format!("missing required field '{key}'")))?
        .extract::<String>()
        .map_err(|_e| PyValueError::new_err(format!("field '{key}' must be a string")))
}

/// 从 Python dict 读取**必填** f64 字段。
fn get_dict_f64(d: &Bound<'_, PyDict>, key: &str) -> PyResult<f64> {
    d.get_item(key)?
        .ok_or_else(|| PyKeyError::new_err(format!("missing required field '{key}'")))?
        .extract::<f64>()
        .map_err(|_e| PyValueError::new_err(format!("field '{key}' must be a float")))
}

/// 从 Python dict 读取**可选**字符串字段(缺字段返回 `None`)。
fn get_dict_optional_string(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    match d.get_item(key)? {
        Some(v) => {
            if v.is_none() {
                Ok(None)
            } else {
                v.extract::<String>()
                    .map(Some)
                    .map_err(|_e| PyValueError::new_err(format!("field '{key}' must be a string")))
            }
        }
        None => Ok(None),
    }
}

/// 从 Python dict 读取**可选** f64 字段(缺字段返回 `None`)。
fn get_dict_optional_f64(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<f64>> {
    match d.get_item(key)? {
        Some(v) => {
            if v.is_none() {
                Ok(None)
            } else {
                v.extract::<f64>()
                    .map(Some)
                    .map_err(|_e| PyValueError::new_err(format!("field '{key}' must be a float")))
            }
        }
        None => Ok(None),
    }
}

/// 把任意 Serialize 类型转 Python dict(`serde_json` round-trip)。
fn report_to_dict<T: serde::Serialize>(py: Python<'_>, value: &T) -> PyResult<Py<PyAny>> {
    let json = serde_json::to_string(value)
        .map_err(|e| PyValueError::new_err(format!("serialize report failed: {e}")))?;
    let dict: Py<PyAny> = py.import("json")?.call_method1("loads", (json,))?.unbind();
    Ok(dict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ComplianceConfig as RustConfig, LiquidityType, OrderType, TradeRecord, TradeSide,
    };
    use chrono::Utc;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn make_test_config() -> RustConfig {
        RustConfig {
            account_id: "test_account".into(),
            base_currency: "USDT".into(),
            large_trade_threshold: 100_000.0,
            position_limit: 1_000_000.0,
            max_portfolio_concentration: 0.4,
            data_retention_years: 7,
            regulators: vec!["SEC".into()],
        }
    }

    fn make_test_trade() -> TradeRecord {
        TradeRecord {
            trade_id: Uuid::new_v4(),
            order_id: Uuid::new_v4(),
            strategy_id: "strat-1".into(),
            symbol: "BTCUSDT".into(),
            side: TradeSide::Buy,
            quantity: 1.0,
            price: 50_000.0,
            notional_value: 50_000.0,
            fee: 50.0,
            fee_currency: "USDT".into(),
            exchange: "Binance".into(),
            execution_time: Utc::now(),
            settlement_time: None,
            status: crate::types::TradeStatus::Filled,
            order_type: OrderType::Market,
            exchange_trade_id: None,
            liquidity: LiquidityType::Taker,
            realized_pnl: None,
            funding_rate: None,
            slippage: None,
            created_at: Utc::now(),
        }
    }

    /// 直接调 Rust `ComplianceModule` API(record_trade 走字典逻辑此处不测,
    /// Python 端字典→`TradeRecord` 转换在 `python::module::record_trade` 中
    /// 完成,行为由 `python/tests/test_compliance_e2e.py` E2E 覆盖)
    #[test]
    fn rust_module_creation() {
        let tmp = TempDir::new().unwrap();
        let m = RustModule::new(make_test_config(), tmp.path()).unwrap();
        assert_eq!(m.trade_count(), 0);
        assert!(m.verify_audit_integrity());
    }

    #[test]
    fn rust_record_trade_basic() {
        let tmp = TempDir::new().unwrap();
        let mut m = RustModule::new(make_test_config(), tmp.path()).unwrap();
        m.record_trade(make_test_trade()).unwrap();
        assert_eq!(m.trade_count(), 1);
        assert_eq!(m.audit_log().len(), 1);
        assert!(m.verify_audit_integrity());
    }

    #[test]
    fn rust_record_trade_negative_quantity_rejected() {
        let tmp = TempDir::new().unwrap();
        let mut m = RustModule::new(make_test_config(), tmp.path()).unwrap();
        let mut t = make_test_trade();
        t.quantity = -1.0;
        assert!(m.record_trade(t).is_err());
    }

    #[test]
    fn rust_query_trades_by_symbol() {
        let tmp = TempDir::new().unwrap();
        let mut m = RustModule::new(make_test_config(), tmp.path()).unwrap();
        m.record_trade(make_test_trade()).unwrap();
        let mut t2 = make_test_trade();
        t2.symbol = "ETHUSDT".into();
        m.record_trade(t2).unwrap();

        let filter = crate::types::TradeFilter {
            symbol: Some("BTCUSDT".into()),
            ..Default::default()
        };
        let trades = m.query_trades(&filter);
        assert_eq!(trades.len(), 1);
    }
}
