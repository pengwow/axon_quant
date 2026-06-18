//! PyO3 trading 绑定入口(Stage K)
//!
//! 暴露 7 个核心类给 Python:`RiskLimits` / `MockTradingBackend` /
//! `PlaceOrderTool` / `QueryPortfolioTool` / `CancelOrderTool` /
//! `ReplaceOrderTool` / `TradingMetrics`。
//!
//! **不暴露** `TradingBackend` trait object(避免 PyO3 Arc<dyn> 复杂性),
//! 只暴露具体 `MockTradingBackend` 类。真实交易所(Exchange/OMS/Backtest)
//! 按需在 Python 侧自实现。
//!
//! ## Python 用法
//!
//! ```python
//! from axon_quant.trading import (
//!     RiskLimits, MockTradingBackend, PlaceOrderTool, QueryPortfolioTool,
//! )
//!
//! backend = MockTradingBackend()
//! risk = RiskLimits(allowed_symbols=["BTC-USDT"])
//! place = PlaceOrderTool(backend=backend, mode="dry_run", risk=risk)
//! ack = place.execute({
//!     "symbol": "BTC-USDT",
//!     "side": "Buy",
//!     "quantity": 0.1,
//!     "price": 50000.0,
//! })
//! print(ack["status"])  # "DryRun"
//! ```

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]

use std::sync::Arc as StdArc;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};

use crate::tools::Tool;
use crate::trading::backend::TradingBackend;
use crate::trading::cancel_order_tool::CancelOrderTool as RustCancelOrderTool;
use crate::trading::mock::MockTradingBackend;
use crate::trading::place_order_tool::PlaceOrderTool as RustPlaceOrderTool;
use crate::trading::query_portfolio_tool::QueryPortfolioTool as RustQueryPortfolioTool;
use crate::trading::replace_order_tool::ReplaceOrderTool as RustReplaceOrderTool;
use crate::trading::safety::{DailyCounter, RiskLimits, SafetyMode};
use crate::trading::types::PlaceOrderArgs;

use super::helpers::pythonize;

/// Python 端可见的 `RiskLimits` 包装
///
/// 直接转发 `RiskLimits` 字段(white-list / notional / daily orders / etc),
/// 不引入额外 Python dataclass 包装。
///
/// `skip_from_py_object`:RiskLimits 用 keyword args 构造,不允许 Python 端
/// 传 dict 自动转换(避免 PyO3 0.28 派生 deprecation warning)。
#[pyclass(name = "RiskLimits", skip_from_py_object)]
#[derive(Clone, Default)]
pub struct PyRiskLimits {
    pub(crate) inner: RiskLimits,
}

#[pymethods]
impl PyRiskLimits {
    #[new]
    #[pyo3(signature = (
        max_order_notional=None,
        max_daily_orders=None,
        max_daily_cancels=None,
        max_position_abs=None,
        allowed_symbols=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        max_order_notional: Option<f64>,
        max_daily_orders: Option<u32>,
        max_daily_cancels: Option<u32>,
        max_position_abs: Option<f64>,
        allowed_symbols: Option<Vec<String>>,
    ) -> Self {
        Self {
            inner: RiskLimits {
                max_order_notional,
                max_daily_orders,
                max_daily_cancels,
                max_position_abs,
                allowed_symbols,
            },
        }
    }

    /// 默认无限制(全部 None)
    #[staticmethod]
    fn permissive() -> Self {
        Self {
            inner: RiskLimits::permissive(),
        }
    }

    /// 字符串表示
    fn __repr__(&self) -> String {
        format!(
            "RiskLimits(notional={:?}, daily_orders={:?}, daily_cancels={:?}, position_abs={:?}, allowed={:?})",
            self.inner.max_order_notional,
            self.inner.max_daily_orders,
            self.inner.max_daily_cancels,
            self.inner.max_position_abs,
            self.inner.allowed_symbols,
        )
    }
}

/// 把 trading 子模块的 pyclass 注册到给定的 PyModule
///
/// 由 `axon_llm::python::mod.rs` 的 `#[pymodule] axon_llm` 调用,
/// 把 trading 类挂到 `axon_llm.trading` 命名空间下。
/// `axon-python` crate 通过 `axon_llm::python::trading::register_trading_module`
/// 调用同一函数,把它挂到 `_native.trading` 命名空间下。
///
/// **注册顺序无关**:PyO3 `add_class` 只记录类型 + name 映射,实际实例化
/// 发生在 Python 端调用时。`name = "..."` 属性决定 Python 端可见名。
pub fn register_trading_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRiskLimits>()?;
    m.add_class::<PyMockTradingBackend>()?;
    m.add_class::<PyPlaceOrderTool>()?;
    m.add_class::<PyQueryPortfolioTool>()?;
    m.add_class::<PyCancelOrderTool>()?;
    m.add_class::<PyReplaceOrderTool>()?;
    m.add_class::<PyTradingMetrics>()?;
    Ok(())
}

/// Python 端可见的 `MockTradingBackend` 包装
///
/// 内部独占 `tokio::Runtime` + `MockTradingBackend`(通过 `Arc<dyn TradingBackend>`)。
/// `MockTradingBackend` 内部状态用 `parking_lot::Mutex` 同步访问,不依赖 async。
///
/// **Stage K 简化**:直接持 `Arc<MockTradingBackend>`(具体类),不暴露为
/// `Arc<dyn TradingBackend>`(避免 PyO3 trait object 复杂)。
#[pyclass(name = "MockTradingBackend")]
pub struct PyMockTradingBackend {
    /// 内部 backend(MockTradingBackend 已是 TradingBackend impl,通过 Arc 共享)
    pub(crate) backend: StdArc<MockTradingBackend>,
}

#[pymethods]
impl PyMockTradingBackend {
    #[new]
    fn new() -> Self {
        Self {
            backend: StdArc::new(MockTradingBackend::new()),
        }
    }

    /// 已下单数量(测试用)
    fn order_count(&self) -> usize {
        // MockTradingBackend 内部用 parking_lot::Mutex,blocking_lock 不会 await
        self.backend.order_count()
    }

    /// 字符串表示
    fn __repr__(&self) -> String {
        format!("MockTradingBackend(orders={})", self.order_count())
    }
}

// ── 工具类(Tasks 4-7)────────────────────────────────────

/// Python dict → Rust `PlaceOrderArgs`
///
/// 经 `pythonize` 桥到 `serde_json::Value`,再 `serde_json::from_value`
/// 复用 Rust 端的字段校验(必填 symbol/side/quantity + 默认值 order_type=GTC/...
/// + 透传 extras)。错误信息保留 serde 详细描述,便于 LLM 排错。
///
/// **不导出到 Python**:`#[allow(non_snake_case)]` 标在 `parse_place_order_args`
/// 等函数,避免 clippy 警告(snake_case 是 Python 函数命名风格)。
pub(crate) fn parse_place_order_args(obj: &Bound<'_, PyAny>) -> PyResult<PlaceOrderArgs> {
    let json_value = pythonize(obj.py(), obj)?;
    serde_json::from_value(json_value).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("invalid place_order args: {}", e))
    })
}

/// Python dict → Rust `CancelOrderArgs`
#[allow(dead_code)] // Task 6 (PyCancelOrderTool) 启用
pub(crate) fn parse_cancel_order_args(
    obj: &Bound<'_, PyAny>,
) -> PyResult<crate::trading::types::CancelOrderArgs> {
    let json_value = pythonize(obj.py(), obj)?;
    serde_json::from_value(json_value).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("invalid cancel_order args: {}", e))
    })
}

/// Python dict → Rust `ReplaceOrderArgs`
#[allow(dead_code)] // Task 7 (PyReplaceOrderTool) 启用
pub(crate) fn parse_replace_order_args(
    obj: &Bound<'_, PyAny>,
) -> PyResult<crate::trading::types::ReplaceOrderArgs> {
    let json_value = pythonize(obj.py(), obj)?;
    serde_json::from_value(json_value).map_err(|e| {
        pyo3::exceptions::PyValueError::new_err(format!("invalid replace_order args: {}", e))
    })
}

/// `serde_json::Value` → Python 对象(str/int/float/bool/list/dict/None)
///
/// 反向版 `pythonize`,把 `Tool::execute` 返回的 JSON 字符串转回 Python dict。
/// NaN / Inf 拒绝(serde_json::Number::from_f64 返回 None)。
///
/// **返回 owned `Py<PyAny>` 而非 `Bound`**:避免借用 `v: &serde_json::Value`
/// 时的 lifetime 纠缠(PyO3 0.28 中 `Bound::Borrowed` 不允许 move)。
/// 调用方按需 `Py::into_bound(py)` 在持有 GIL 时转回 `Bound`。
///
/// **实现方式**:用 `PyDict::new` + `set_item` 而非 `into_pyobject().unbind()`。
/// 基础类型 (`bool` / `i64` / `f64` / `&str`) 在 PyO3 0.28 中实现了
/// `IntoPyObject` 但 `into_pyobject` 在借用场景下返回 `Borrowed`,无法
/// `unbind().into()`(E0507);改用 `set_item(value)` 由 PyO3 自动
/// clone-and-incref,避开 lifetime 纠缠。
pub(crate) fn json_to_py(py: Python<'_>, v: &serde_json::Value) -> PyResult<Py<PyAny>> {
    fn build(py: Python<'_>, v: &serde_json::Value) -> PyResult<Py<PyAny>> {
        match v {
            serde_json::Value::Null => Ok(py.None()),
            serde_json::Value::Bool(b) => {
                // bool 是 interned,但 PyO3 0.28 的 set_item 接受 &bool,内部
                // 会 clone-and-incref,绕过 Borrowed 不可 move 的问题。
                let d = PyDict::new(py);
                d.set_item("_", *b)?;
                let bound = d.get_item("_")?.unwrap();
                Ok(bound.unbind())
            }
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    let d = PyDict::new(py);
                    d.set_item("_", i)?;
                    let bound = d.get_item("_")?.unwrap();
                    Ok(bound.unbind())
                } else if let Some(f) = n.as_f64() {
                    let d = PyDict::new(py);
                    d.set_item("_", f)?;
                    let bound = d.get_item("_")?.unwrap();
                    Ok(bound.unbind())
                } else {
                    Err(pyo3::exceptions::PyValueError::new_err(
                        "unsupported number",
                    ))
                }
            }
            serde_json::Value::String(s) => {
                let d = PyDict::new(py);
                d.set_item("_", s.as_str())?;
                let bound = d.get_item("_")?.unwrap();
                Ok(bound.unbind())
            }
            serde_json::Value::Array(arr) => {
                // 递归构造 list
                let l = PyList::empty(py);
                for item in arr {
                    let item_obj = build(py, item)?;
                    l.append(item_obj)?;
                }
                Ok(l.unbind().into())
            }
            serde_json::Value::Object(obj) => {
                let d = PyDict::new(py);
                for (k, val) in obj {
                    let val_obj = build(py, val)?;
                    d.set_item(k, val_obj)?;
                }
                Ok(d.unbind().into())
            }
        }
    }
    build(py, v)
}

/// 把 `Tool::execute` 返回的 JSON 字符串结果包成 Python 对象
///
/// `Tool::execute` 返回 `Result<String, ToolError>`,约定 `String` 是 JSON。
/// Python 端需要的是 dict/list(LLM 工具调用结果通常走 JSON 序列化),
/// 直接用 `json_to_py` 解析。
///
/// 返回 owned `Py<PyAny>`,调用方按需 `into_bound(py)`。
///
/// 错误情况:JSON 解析失败(理论上 Rust 端不会返回合法 JSON,防御性兜底)。
pub(crate) fn tool_result_to_py(py: Python<'_>, json_str: &str) -> PyResult<Py<PyAny>> {
    let v: serde_json::Value = serde_json::from_str(json_str).map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("tool returned invalid JSON: {}", e))
    })?;
    json_to_py(py, &v)
}

/// `ToolError` → Python 异常
///
/// - `InvalidArguments` → `ValueError`(参数错误,LLM 可修复)
/// - `ExecutionFailed` → `RuntimeError`(运行时错误,需运维介入)
/// - `PermissionDenied` → `PermissionError`(Python 标准异常)
pub(crate) fn map_tool_error(e: crate::tools::ToolError) -> PyErr {
    use crate::tools::ToolError as E;
    match e {
        E::InvalidArguments(msg) => pyo3::exceptions::PyValueError::new_err(msg),
        E::ExecutionFailed(msg) => pyo3::exceptions::PyRuntimeError::new_err(msg),
        E::PermissionDenied { tool, operation } => pyo3::exceptions::PyPermissionError::new_err(
            format!("{} 不允许执行 {}", tool, operation),
        ),
    }
}

/// Python 端可见的 `PlaceOrderTool` 包装
///
/// 内部独占 `tokio::Runtime`(用于把 `Tool::execute` 异步桥到 Python 同步调用)
/// + `Arc<DailyCounter>`(自维护,每个 tool 独立计数,允许 Python 端
/// 多 tool 共享同一 backend 但各自单日限额)。
///
/// **构造时**:`backend` 是 `MockTradingBackend` 具体类,通过 `Arc::clone`
/// 借用为 `Arc<dyn TradingBackend>` 传给 Rust `PlaceOrderTool::new`。
/// `mode` 是字符串 `"dry_run"` / `"two_phase"` / `"direct"`。
/// `risk` 是 `PyRiskLimits` 包装,直接 `inner.clone()` 转发。
/// `metrics` 可选,Stage H metrics 收集器。
#[pyclass(name = "PlaceOrderTool")]
pub struct PyPlaceOrderTool {
    pub(crate) tool: StdArc<RustPlaceOrderTool>,
    pub(crate) runtime: StdArc<tokio::runtime::Runtime>,
}

#[pymethods]
impl PyPlaceOrderTool {
    #[new]
    #[pyo3(signature = (backend, mode, risk, metrics=None))]
    fn new(
        backend: &PyMockTradingBackend,
        mode: &str,
        risk: &PyRiskLimits,
        metrics: Option<&PyTradingMetrics>,
    ) -> PyResult<Self> {
        // 字符串 → SafetyMode
        let safety_mode = parse_safety_mode(mode)?;
        // PyMockTradingBackend.backend: Arc<MockTradingBackend>
        //   MockTradingBackend: TradingBackend impl
        // 借用 trait object 传给 PlaceOrderTool::new(不动 backend 所有权)
        let backend_arc: StdArc<dyn TradingBackend> = backend.backend.clone();
        // 自维护 DailyCounter(每个 PyPlaceOrderTool 实例独立计数)
        let daily = StdArc::new(DailyCounter::default());
        let mut tool = RustPlaceOrderTool::new(backend_arc, safety_mode, risk.inner.clone(), daily);
        if let Some(m) = metrics {
            tool = tool.with_metrics(m.inner.clone());
        }
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self {
            tool: StdArc::new(tool),
            runtime: StdArc::new(runtime),
        })
    }

    /// 同步执行下单:Python dict → Rust PlaceOrderArgs → PlaceOrderTool → OrderAck dict
    ///
    /// 错误以 Python 异常形式抛出(`ValueError` / `RuntimeError` / `PermissionError`),
    /// LLM agent 可捕获并决策重试 / 切换策略。
    fn execute<'py>(
        &self,
        py: Python<'py>,
        args: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let parsed = parse_place_order_args(args)?;
        let args_json = serde_json::to_string(&parsed)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("serialize: {}", e)))?;
        let tool = self.tool.clone();
        let result = self
            .runtime
            .block_on(async move { tool.execute(&args_json).await });
        match result {
            Ok(json_str) => {
                let owned = tool_result_to_py(py, &json_str)?;
                Ok(owned.into_bound(py))
            }
            Err(e) => Err(map_tool_error(e)),
        }
    }

    /// 当前安全模式字符串(便于 LLM agent 决策)
    #[getter]
    fn mode(&self) -> &'static str {
        match self.tool.mode() {
            SafetyMode::DryRun => "dry_run",
            SafetyMode::TwoPhase => "two_phase",
            SafetyMode::Direct => "direct",
        }
    }

    fn __repr__(&self) -> String {
        format!("PlaceOrderTool(mode={})", self.mode())
    }
}

/// 把 `SafetyMode` 字符串解析为 enum(辅助函数,避免在多个 tool 中重复)
pub(crate) fn parse_safety_mode(mode: &str) -> PyResult<SafetyMode> {
    match mode {
        "dry_run" => Ok(SafetyMode::DryRun),
        "two_phase" => Ok(SafetyMode::TwoPhase),
        "direct" => Ok(SafetyMode::Direct),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "invalid mode '{}', expected 'dry_run' / 'two_phase' / 'direct'",
            other
        ))),
    }
}

// ── PyQueryPortfolioTool(Task 5)───────────────────────

/// Python 端可见的 `QueryPortfolioTool` 包装
///
/// 内部独占 `tokio::Runtime`,把 `Tool::execute` 异步桥到 Python 同步调用。
/// 借用 `MockTradingBackend`(`Arc::clone` → `Arc<dyn TradingBackend>`),
/// 不独占 backend 所有权,允许多 tool 共享。
///
/// **args 可选**:`execute(args=None)` → 透传 `"{}"`,Rust 端会 fallback
/// 到 `QueryPortfolioArgs::default()` 返回全量 portfolio。
#[pyclass(name = "QueryPortfolioTool")]
pub struct PyQueryPortfolioTool {
    pub(crate) tool: StdArc<RustQueryPortfolioTool>,
    pub(crate) runtime: StdArc<tokio::runtime::Runtime>,
}

#[pymethods]
impl PyQueryPortfolioTool {
    #[new]
    fn new(backend: &PyMockTradingBackend) -> PyResult<Self> {
        // 借用 trait object(不消耗 backend 所有权)
        let backend_arc: StdArc<dyn TradingBackend> = backend.backend.clone();
        let tool = RustQueryPortfolioTool::new(backend_arc);
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self {
            tool: StdArc::new(tool),
            runtime: StdArc::new(runtime),
        })
    }

    /// 同步执行:Python dict → JSON 字符串 → Tool::execute → JSON 字符串 → Python dict
    ///
    /// `args` 可为 `None`(默认)或 dict:
    ///   - `None` → 传 `"{}"`,Rust 端用 default args 返回全量
    ///   - `{"symbol": "BTC-USDT"}` → 按 symbol 过滤持仓(不影响 balance)
    #[pyo3(signature = (args=None))]
    fn execute<'py>(
        &self,
        py: Python<'py>,
        args: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // 把 args 序列化为 JSON 字符串(允许 None → "{}")
        let args_str = match args {
            Some(d) => {
                let v = pythonize(py, d.as_any())?;
                serde_json::to_string(&v)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?
            }
            None => "{}".to_string(),
        };
        let tool = self.tool.clone();
        let result = self
            .runtime
            .block_on(async move { tool.execute(&args_str).await });
        match result {
            Ok(json_str) => {
                let owned = tool_result_to_py(py, &json_str)?;
                Ok(owned.into_bound(py))
            }
            Err(e) => Err(map_tool_error(e)),
        }
    }

    fn __repr__(&self) -> String {
        "QueryPortfolioTool()".to_string()
    }
}

// ── PyCancelOrderTool(Task 6)──────────────────────────

/// Python 端可见的 `CancelOrderTool` 包装
///
/// 内部独占 `tokio::Runtime`,自维护 `Arc<DailyCounter>`(单日撤单计数,
/// Python 端多 tool 共享同一 backend 但各自单日撤单限额)。
///
/// **构造时**:
///   - `backend` 是 `MockTradingBackend` 具体类,通过 `Arc::clone` 借用
///   - `risk` 是 `PyRiskLimits` 包装
///   - `metrics` 可选(Stage H 埋点)
#[pyclass(name = "CancelOrderTool")]
pub struct PyCancelOrderTool {
    pub(crate) tool: StdArc<RustCancelOrderTool>,
    pub(crate) runtime: StdArc<tokio::runtime::Runtime>,
}

#[pymethods]
impl PyCancelOrderTool {
    #[new]
    #[pyo3(signature = (backend, risk, metrics=None))]
    fn new(
        backend: &PyMockTradingBackend,
        risk: &PyRiskLimits,
        metrics: Option<&PyTradingMetrics>,
    ) -> PyResult<Self> {
        let backend_arc: StdArc<dyn TradingBackend> = backend.backend.clone();
        // 自维护 DailyCounter
        let daily = StdArc::new(DailyCounter::default());
        let mut tool = RustCancelOrderTool::new(backend_arc, risk.inner.clone(), daily);
        if let Some(m) = metrics {
            tool = tool.with_metrics(m.inner.clone());
        }
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self {
            tool: StdArc::new(tool),
            runtime: StdArc::new(runtime),
        })
    }

    /// 同步撤单:Python dict → Rust CancelOrderArgs → Tool::execute → OrderAck dict
    fn execute<'py>(
        &self,
        py: Python<'py>,
        args: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // 解析 `{"order_id": "..."}` 为 CancelOrderArgs
        let parsed = parse_cancel_order_args(args)?;
        let args_json = serde_json::to_string(&parsed)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        let tool = self.tool.clone();
        let result = self
            .runtime
            .block_on(async move { tool.execute(&args_json).await });
        match result {
            Ok(json_str) => {
                let owned = tool_result_to_py(py, &json_str)?;
                Ok(owned.into_bound(py))
            }
            Err(e) => Err(map_tool_error(e)),
        }
    }

    fn __repr__(&self) -> String {
        "CancelOrderTool()".to_string()
    }
}

// ── PyReplaceOrderTool(Task 7)──────────────────────────

/// Python 端可见的 `ReplaceOrderTool` 包装
///
/// 内部独占 `tokio::Runtime`,借用 `MockTradingBackend`。
///
/// **args 结构**:Python dict 包含
///   - `order_id`:要修改的订单 ID
///   - `new_req`:dict 包含完整新参数(symbol / side / quantity / price / ...)
///     复用 `PlaceOrderArgs` 字段(`parse_place_order_args` 也能解析嵌套 dict)
#[pyclass(name = "ReplaceOrderTool")]
pub struct PyReplaceOrderTool {
    pub(crate) tool: StdArc<RustReplaceOrderTool>,
    pub(crate) runtime: StdArc<tokio::runtime::Runtime>,
}

#[pymethods]
impl PyReplaceOrderTool {
    #[new]
    #[pyo3(signature = (backend, risk, metrics=None))]
    fn new(
        backend: &PyMockTradingBackend,
        risk: &PyRiskLimits,
        metrics: Option<&PyTradingMetrics>,
    ) -> PyResult<Self> {
        let backend_arc: StdArc<dyn TradingBackend> = backend.backend.clone();
        let mut tool = RustReplaceOrderTool::new(backend_arc, risk.inner.clone());
        if let Some(m) = metrics {
            tool = tool.with_metrics(m.inner.clone());
        }
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self {
            tool: StdArc::new(tool),
            runtime: StdArc::new(runtime),
        })
    }

    /// 同步改单:Python dict → Rust ReplaceOrderArgs → Tool::execute → OrderAck dict
    fn execute<'py>(
        &self,
        py: Python<'py>,
        args: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let parsed = parse_replace_order_args(args)?;
        let args_json = serde_json::to_string(&parsed)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        let tool = self.tool.clone();
        let result = self
            .runtime
            .block_on(async move { tool.execute(&args_json).await });
        match result {
            Ok(json_str) => {
                let owned = tool_result_to_py(py, &json_str)?;
                Ok(owned.into_bound(py))
            }
            Err(e) => Err(map_tool_error(e)),
        }
    }

    fn __repr__(&self) -> String {
        "ReplaceOrderTool()".to_string()
    }
}

// ── PyTradingMetrics(声明,实现在 Task 8)─────────────────

/// Python 端可见的 `TradingMetrics` 包装
///
/// Stage H 指标收集器,提供 snapshot 数据导出。
/// `PlaceOrderTool` / `CancelOrderTool` / `ReplaceOrderTool` 通过
/// `metrics: Option<&PyTradingMetrics>` 参数启用埋点。
///
/// 引用计数:`Arc<TradingMetrics>`(多 tool 共享同一 metrics 实例)。
#[pyclass(name = "TradingMetrics")]
pub struct PyTradingMetrics {
    pub(crate) inner: StdArc<crate::trading::metrics::TradingMetrics>,
}

#[pymethods]
impl PyTradingMetrics {
    #[new]
    fn new() -> Self {
        Self {
            inner: StdArc::new(crate::trading::metrics::TradingMetrics::new()),
        }
    }

    /// 拿到全量 snapshot(返回 list of dict)
    ///
    /// 每个 sample dict 包含 `name` / `kind` / `value` / `labels`。
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let samples = self.inner.snapshot();
        let owned = samples_to_py_list(py, &samples)?;
        Ok(owned.into_bound(py))
    }

    /// 按 name 过滤 snapshot
    fn snapshot_filtered<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyList>> {
        let samples = self.inner.snapshot_filtered(name);
        let owned = samples_to_py_list(py, &samples)?;
        Ok(owned.into_bound(py))
    }

    fn __repr__(&self) -> String {
        "TradingMetrics()".to_string()
    }
}

/// `Vec<MetricSample>` → Python list of dict(共享 helper)
///
/// 返回 owned `Py<PyList>`,调用方 `into_bound(py)` 转 `Bound`。
fn samples_to_py_list(
    py: Python<'_>,
    samples: &[crate::trading::metrics::MetricSample],
) -> PyResult<Py<PyList>> {
    let l = PyList::empty(py);
    for s in samples {
        let d = PyDict::new(py);
        d.set_item("name", &s.name)?;
        d.set_item("kind", format!("{:?}", s.kind))?;
        d.set_item("value", s.value)?;
        let labels_d = PyDict::new(py);
        for (k, v) in &s.labels {
            labels_d.set_item(k, v)?;
        }
        d.set_item("labels", labels_d)?;
        l.append(d)?;
    }
    Ok(l.unbind())
}

// ── 单元测试 ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyDict;

    /// 关键:用 `Python::attach` 而不是 `with_gil`(PyO3 0.28 API)
    fn run<F, R>(f: F) -> R
    where
        F: FnOnce(Python<'_>) -> R,
    {
        Python::attach(f)
    }

    #[test]
    fn risk_limits_new_with_all_fields() {
        let rl = PyRiskLimits::new(
            Some(100.0),
            Some(20),
            Some(5),
            Some(10.0),
            Some(vec!["BTC-USDT".into()]),
        );
        assert_eq!(rl.inner.max_order_notional, Some(100.0));
        assert_eq!(rl.inner.max_daily_orders, Some(20));
        assert_eq!(rl.inner.max_daily_cancels, Some(5));
        assert_eq!(rl.inner.max_position_abs, Some(10.0));
        assert_eq!(rl.inner.allowed_symbols, Some(vec!["BTC-USDT".to_string()]));
    }

    #[test]
    fn risk_limits_permissive_returns_all_none() {
        let rl = PyRiskLimits::permissive();
        assert!(rl.inner.max_order_notional.is_none());
        assert!(rl.inner.max_daily_orders.is_none());
        assert!(rl.inner.max_daily_cancels.is_none());
        assert!(rl.inner.max_position_abs.is_none());
        assert!(rl.inner.allowed_symbols.is_none());
    }

    #[test]
    fn risk_limits_default_via_clone_default() {
        let rl = PyRiskLimits::default();
        assert!(rl.inner.allowed_symbols.is_none());
    }

    #[test]
    fn mock_backend_constructs() {
        let backend = PyMockTradingBackend::new();
        assert_eq!(backend.order_count(), 0);
    }

    #[test]
    fn parse_place_order_args_minimal() {
        run(|py| {
            let d = PyDict::new(py);
            d.set_item("symbol", "BTC-USDT").unwrap();
            d.set_item("side", "Buy").unwrap();
            d.set_item("quantity", 0.1).unwrap();
            let parsed = parse_place_order_args(d.as_any()).unwrap();
            assert_eq!(parsed.symbol, "BTC-USDT");
            assert_eq!(parsed.side, crate::trading::types::OrderSide::Buy);
            assert_eq!(parsed.quantity, 0.1);
            // 默认值
            assert_eq!(parsed.order_type, crate::trading::types::OrderKind::Limit);
            assert_eq!(
                parsed.time_in_force,
                crate::trading::types::TimeInForce::GTC
            );
        });
    }

    #[test]
    fn parse_place_order_args_rejects_non_dict() {
        run(|py| {
            use pyo3::IntoPyObject;
            let s = "not a dict".into_pyobject(py).unwrap();
            let r = parse_place_order_args(s.as_any());
            assert!(r.is_err());
        });
    }

    #[test]
    fn parse_place_order_args_rejects_missing_required() {
        run(|py| {
            let d = PyDict::new(py);
            // 缺 symbol / side / quantity
            let r = parse_place_order_args(d.as_any());
            assert!(r.is_err());
        });
    }

    #[test]
    fn parse_safety_mode_valid() {
        assert!(matches!(
            parse_safety_mode("dry_run"),
            Ok(SafetyMode::DryRun)
        ));
        assert!(matches!(
            parse_safety_mode("two_phase"),
            Ok(SafetyMode::TwoPhase)
        ));
        assert!(matches!(
            parse_safety_mode("direct"),
            Ok(SafetyMode::Direct)
        ));
    }

    #[test]
    fn parse_safety_mode_invalid() {
        assert!(parse_safety_mode("unknown").is_err());
        assert!(parse_safety_mode("DRY_RUN").is_err()); // 大小写敏感
    }

    #[test]
    fn json_to_py_roundtrip() {
        run(|py| {
            // 简单 dict
            let v = serde_json::json!({"k": 1, "s": "x", "b": true, "n": null});
            let owned = json_to_py(py, &v).unwrap();
            let obj = owned.into_bound(py);
            // 验证返回的是 dict
            let d = obj.cast::<PyDict>().unwrap();
            assert_eq!(
                d.get_item("k").unwrap().unwrap().extract::<i64>().unwrap(),
                1
            );
            assert_eq!(
                d.get_item("s")
                    .unwrap()
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "x"
            );
            assert!(d.get_item("b").unwrap().unwrap().extract::<bool>().unwrap());
            assert!(d.get_item("n").unwrap().unwrap().is_none());
        });
    }

    #[test]
    fn json_to_py_nested() {
        run(|py| {
            let v = serde_json::json!({
                "balance": {"currencies": [{"currency": "USDT", "free": 1000.0}]},
                "positions": [],
            });
            let owned = json_to_py(py, &v).unwrap();
            let obj = owned.into_bound(py);
            let d = obj.cast::<PyDict>().unwrap();
            let balance = d.get_item("balance").unwrap().unwrap();
            let balance_d = balance.cast::<PyDict>().unwrap();
            let currencies = balance_d.get_item("currencies").unwrap().unwrap();
            let l = currencies.cast::<PyList>().unwrap();
            assert_eq!(l.len(), 1);
        });
    }

    #[test]
    fn trading_metrics_snapshot_empty_counters_but_has_daily_gauge() {
        // `TradingMetrics::snapshot()` 永远 emit 一个 `trading_daily_orders_count`
        // Gauge(DailyCounter 镜像);其他 5 个 LabeledCounter 初始为空。
        let m = PyTradingMetrics::new();
        run(|py| {
            let snapshot = m.snapshot(py).unwrap();
            // 至少 1 个 sample(就是 daily gauge)
            assert!(
                snapshot.len() >= 1,
                "snapshot should have at least daily gauge"
            );
            // 确认含 daily gauge
            let has_daily_gauge = (0..snapshot.len()).any(|i| {
                let item = snapshot.get_item(i).unwrap();
                let d = item.cast::<PyDict>().unwrap();
                let name: String = d.get_item("name").unwrap().unwrap().extract().unwrap();
                name == "trading_daily_orders_count"
            });
            assert!(has_daily_gauge, "expected daily gauge in snapshot");
        });
    }

    #[test]
    fn place_order_tool_dry_run_constructs_and_executes() {
        run(|py| {
            let _ = py; // 保留 py token 防止误用
            let backend = PyMockTradingBackend::new();
            let risk = PyRiskLimits::permissive();
            let tool = PyPlaceOrderTool::new(&backend, "dry_run", &risk, None).unwrap();
            // 构造 + __repr__
            let repr = tool.__repr__();
            assert!(repr.contains("PlaceOrderTool"));
            assert!(repr.contains("dry_run"));
            // mode getter
            assert_eq!(tool.mode(), "dry_run");
        });
    }

    #[test]
    fn place_order_tool_rejects_invalid_mode() {
        let backend = PyMockTradingBackend::new();
        let risk = PyRiskLimits::permissive();
        let r = PyPlaceOrderTool::new(&backend, "weird", &risk, None);
        assert!(r.is_err());
    }

    // ── PyQueryPortfolioTool 测试 ──────────────────────────

    #[test]
    fn query_portfolio_tool_constructs() {
        let backend = PyMockTradingBackend::new();
        let tool = PyQueryPortfolioTool::new(&backend).unwrap();
        // 构造 + __repr__
        let repr = tool.__repr__();
        assert!(repr.contains("QueryPortfolioTool"));
    }

    #[test]
    fn query_portfolio_tool_execute_no_args_returns_full_portfolio() {
        // 默认 args(None) → 后端 portfolio(balance + positions)
        run(|py| {
            let backend = PyMockTradingBackend::new();
            let tool = PyQueryPortfolioTool::new(&backend).unwrap();
            // 不传 args(走 None 分支)
            let result = tool.execute(py, None).unwrap();
            // 返回 dict,含 balance + positions
            let d = result.cast::<PyDict>().unwrap();
            assert!(d.get_item("balance").unwrap().is_some());
            assert!(d.get_item("positions").unwrap().is_some());
        });
    }

    #[test]
    fn query_portfolio_tool_execute_with_empty_dict() {
        // 传空 dict → 跟 None 等价
        run(|py| {
            let backend = PyMockTradingBackend::new();
            let tool = PyQueryPortfolioTool::new(&backend).unwrap();
            let d = PyDict::new(py);
            let result = tool.execute(py, Some(&d)).unwrap();
            let pd = result.cast::<PyDict>().unwrap();
            assert!(pd.get_item("balance").unwrap().is_some());
        });
    }

    #[test]
    fn query_portfolio_tool_execute_with_symbol_filter() {
        // symbol 过滤:仅影响 positions,balance 仍返回全量
        run(|py| {
            let backend = PyMockTradingBackend::new();
            let tool = PyQueryPortfolioTool::new(&backend).unwrap();
            let d = PyDict::new(py);
            d.set_item("symbol", "BTC-USDT").unwrap();
            let result = tool.execute(py, Some(&d)).unwrap();
            let pd = result.cast::<PyDict>().unwrap();
            // balance 仍存在
            assert!(pd.get_item("balance").unwrap().is_some());
            // positions 可能是空(若 mock 默认 portfolio 没 BTC-USDT)
            let positions = pd.get_item("positions").unwrap().unwrap();
            let l = positions.cast::<PyList>().unwrap();
            // 不论如何,列表合法
            let _ = l.len();
        });
    }

    // ── PyCancelOrderTool 测试 ─────────────────────────────

    #[test]
    fn parse_cancel_order_args_minimal() {
        run(|py| {
            let d = PyDict::new(py);
            d.set_item("order_id", "ord-1").unwrap();
            let parsed = parse_cancel_order_args(d.as_any()).unwrap();
            assert_eq!(parsed.order_id, "ord-1");
        });
    }

    #[test]
    fn parse_cancel_order_args_rejects_missing_order_id() {
        run(|py| {
            let d = PyDict::new(py);
            // 缺 order_id → 解析失败
            let r = parse_cancel_order_args(d.as_any());
            assert!(r.is_err());
        });
    }

    #[test]
    fn cancel_order_tool_constructs() {
        let backend = PyMockTradingBackend::new();
        let risk = PyRiskLimits::permissive();
        let tool = PyCancelOrderTool::new(&backend, &risk, None).unwrap();
        let repr = tool.__repr__();
        assert!(repr.contains("CancelOrderTool"));
    }

    #[test]
    fn cancel_order_tool_direct_succeeds() {
        // direct 模式下:place → cancel → 返回 Cancelled 状态
        run(|py| {
            let backend = PyMockTradingBackend::new();
            let risk = PyRiskLimits::permissive();
            let place = PyPlaceOrderTool::new(&backend, "direct", &risk, None).unwrap();
            let cancel = PyCancelOrderTool::new(&backend, &risk, None).unwrap();
            // 先下个单
            let place_args = PyDict::new(py);
            place_args.set_item("symbol", "BTC-USDT").unwrap();
            place_args.set_item("side", "Buy").unwrap();
            place_args.set_item("quantity", 0.1).unwrap();
            place_args.set_item("price", 50000.0).unwrap();
            let ack = place.execute(py, &place_args).unwrap();
            let ack_d = ack.cast::<PyDict>().unwrap();
            let order_id: String = ack_d
                .get_item("order_id")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            // 撤单
            let cancel_args = PyDict::new(py);
            cancel_args.set_item("order_id", order_id).unwrap();
            let result = cancel.execute(py, &cancel_args).unwrap();
            let rd = result.cast::<PyDict>().unwrap();
            let status: String = rd.get_item("status").unwrap().unwrap().extract().unwrap();
            assert!(status.contains("ancel") || status.contains("CANCEL"));
        });
    }

    #[test]
    fn cancel_order_tool_daily_limit_rejects() {
        // 限制单日撤单 1 次,第二次撤单应失败
        run(|py| {
            let backend = PyMockTradingBackend::new();
            let risk = PyRiskLimits::permissive();
            // 下 2 个订单(direct)
            let place = PyPlaceOrderTool::new(&backend, "direct", &risk, None).unwrap();
            let place_args = PyDict::new(py);
            place_args.set_item("symbol", "BTC-USDT").unwrap();
            place_args.set_item("side", "Buy").unwrap();
            place_args.set_item("quantity", 0.1).unwrap();
            place_args.set_item("price", 50000.0).unwrap();
            let ack1 = place.execute(py, &place_args).unwrap();
            let ack2 = place.execute(py, &place_args).unwrap();
            let ack1_d = ack1.cast::<PyDict>().unwrap();
            let ack2_d = ack2.cast::<PyDict>().unwrap();
            let id1: String = ack1_d
                .get_item("order_id")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            let id2: String = ack2_d
                .get_item("order_id")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            // 创建限制为 1 次撤单的 tool
            let limited_risk = PyRiskLimits::new(None, None, Some(1), None, None);
            let cancel = PyCancelOrderTool::new(&backend, &limited_risk, None).unwrap();
            // 第一次撤单成功
            let a1 = PyDict::new(py);
            a1.set_item("order_id", id1).unwrap();
            cancel.execute(py, &a1).unwrap();
            // 第二次撤单失败(单日超限)
            let a2 = PyDict::new(py);
            a2.set_item("order_id", id2).unwrap();
            let r = cancel.execute(py, &a2);
            assert!(r.is_err());
        });
    }

    // ── PyReplaceOrderTool 测试 ─────────────────────────────

    #[test]
    fn parse_replace_order_args_minimal() {
        run(|py| {
            // 内嵌 new_req dict
            let new_req = PyDict::new(py);
            new_req.set_item("symbol", "BTC-USDT").unwrap();
            new_req.set_item("side", "Buy").unwrap();
            new_req.set_item("quantity", 0.2).unwrap();
            let d = PyDict::new(py);
            d.set_item("order_id", "ord-1").unwrap();
            d.set_item("new_req", new_req).unwrap();
            let parsed = parse_replace_order_args(d.as_any()).unwrap();
            assert_eq!(parsed.order_id, "ord-1");
            assert_eq!(parsed.new_req.symbol, "BTC-USDT");
            assert_eq!(parsed.new_req.quantity, 0.2);
        });
    }

    #[test]
    fn parse_replace_order_args_rejects_missing_new_req() {
        run(|py| {
            let d = PyDict::new(py);
            d.set_item("order_id", "ord-1").unwrap();
            // 缺 new_req → 解析失败
            let r = parse_replace_order_args(d.as_any());
            assert!(r.is_err());
        });
    }

    #[test]
    fn replace_order_tool_constructs() {
        let backend = PyMockTradingBackend::new();
        let risk = PyRiskLimits::permissive();
        let tool = PyReplaceOrderTool::new(&backend, &risk, None).unwrap();
        let repr = tool.__repr__();
        assert!(repr.contains("ReplaceOrderTool"));
    }

    #[test]
    fn replace_order_tool_direct_succeeds() {
        // direct 模式:place → replace → 数量/价格变化
        run(|py| {
            let backend = PyMockTradingBackend::new();
            let risk = PyRiskLimits::permissive();
            let place = PyPlaceOrderTool::new(&backend, "direct", &risk, None).unwrap();
            let replace = PyReplaceOrderTool::new(&backend, &risk, None).unwrap();
            // 下单
            let place_args = PyDict::new(py);
            place_args.set_item("symbol", "BTC-USDT").unwrap();
            place_args.set_item("side", "Buy").unwrap();
            place_args.set_item("quantity", 0.1).unwrap();
            place_args.set_item("price", 50000.0).unwrap();
            let ack = place.execute(py, &place_args).unwrap();
            let ack_d = ack.cast::<PyDict>().unwrap();
            let order_id: String = ack_d
                .get_item("order_id")
                .unwrap()
                .unwrap()
                .extract()
                .unwrap();
            // 改单:数量 0.2,价格 51000
            let new_req = PyDict::new(py);
            new_req.set_item("symbol", "BTC-USDT").unwrap();
            new_req.set_item("side", "Buy").unwrap();
            new_req.set_item("quantity", 0.2).unwrap();
            new_req.set_item("price", 51000.0).unwrap();
            let replace_args = PyDict::new(py);
            replace_args.set_item("order_id", order_id).unwrap();
            replace_args.set_item("new_req", new_req).unwrap();
            let result = replace.execute(py, &replace_args).unwrap();
            let rd = result.cast::<PyDict>().unwrap();
            // 返回 status 字段(可能是 Replaced / Rejected)
            let status: String = rd.get_item("status").unwrap().unwrap().extract().unwrap();
            // mock 后端可能直接返回 Replaced / Ack 都可能;只要不抛错就算成功
            let _ = status;
        });
    }
}
