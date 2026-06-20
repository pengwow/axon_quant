//! `RiskError` → `PyRiskError(PyException)` 统一异常转换。
//!
//! 设计:与 `axon-backtest::python::error` 保持一致 ——
//! - `RiskError` 继承 builtin `PyException`(不引 `AxonError` 基类,
//!   避免 `axon-risk` 反向依赖 `axon-python` 造成 cargo 循环);
//! - 用 `From<RiskError> for PyErr` 让 `?` 自动转换;
//! - `code` 标签从变体反推,保留 4 个变体的可识别性;`[Code] message`
//!   形式 message 便于 Python 端 `e.args[1].startswith(f"[{code}]")`
//!   二次校验。
//!
//! Python 端使用示例:
//! ```python
//! try:
//!     engine.update_daily_pnl(-1e9)
//! except _native.risk.RiskError as e:
//!     code, message = e.args
//!     if code == "CircuitBreakerActive":
//!         ...
//! ```

use pyo3::exceptions::PyException;
use pyo3::prelude::*;

use crate::error::RiskError as RustRiskError;

// `axon_quant._native.risk.RiskError` —— 继承 builtin `PyException`。
//
// Python 端用 `__module__ = "axon_quant._native.risk"`,但实际 Python 类路径
// 由 `register` 时挂载的位置决定(`_native.risk.RiskError`)。
// (注:`create_exception!` 是宏,上面的 doc 注释必须用 `//` 而非 `///`,
// 否则 rustdoc 报 `unused_doc_comments` 警告 —— 宏展开的 token 不继承 doc。)
pyo3::create_exception!(
    axon_quant._native.risk,
    RiskError,
    PyException,
    "axon-risk specific error. Inherits Exception. \
     `args[0]` is a stable error code (e.g. \"CircuitBreakerActive\" or \"OrderRejected\"); \
     `args[1]` is a human-readable message in the form `[<code>] <details>`."
);

/// 把 Rust `RiskError` 转 Python 异常。
///
/// 设计:必须从变体反推 `code`,保留每个变体的可识别性。
pub fn to_py_err(err: RustRiskError) -> PyErr {
    // 反推稳定错误码(对应 Python 端 `args[0]`)
    let code = match &err {
        RustRiskError::CircuitBreakerActive { .. } => "CircuitBreakerActive",
        RustRiskError::OrderRejected { .. } => "OrderRejected",
        RustRiskError::ConfigInvalid(_) => "ConfigInvalid",
        RustRiskError::Overflow(_) => "Overflow",
    };
    let msg = format!("[{code}] {err}");
    RiskError::new_err((code, msg))
}

impl From<RustRiskError> for PyErr {
    fn from(err: RustRiskError) -> Self {
        to_py_err(err)
    }
}

/// 在 `_native.risk` 子模块下注册 `RiskError` 异常类。
///
/// 调用方:`crates/axon-risk/src/python/mod.rs::register_module`。
///
/// 实现:用 `py.get_type::<RiskError>()` 拿到 PyType,
/// 然后 `parent.add("RiskError", py_type)` 挂到子模块上。
/// 这样不依赖 `axon-python` 的 `_native` Rust 模块,
/// 也避免在 `axon-risk` 中加一个虚拟的 `#[pymodule] fn _native`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    parent.add("RiskError", py.get_type::<RiskError>())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RiskReason;
    use pyo3::Python;

    /// `to_py_err` 反推的 `code` 必须出现在 message 中(`[Code] ...` 形式),
    /// 便于 Python 端 `e.args[1].startswith(f"[{code}]")` 二次校验。
    #[test]
    fn to_py_err_circuit_breaker_preserves_code() {
        Python::attach(|py| {
            let err = RustRiskError::CircuitBreakerActive {
                until: 1_234_567_890,
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[CircuitBreakerActive]"),
                "expected `[CircuitBreakerActive]` in message, got: {s}"
            );
        });
    }

    /// `OrderRejected` 变体能正确转 `PyErr`,code = `"OrderRejected"`。
    #[test]
    fn to_py_err_order_rejected_preserves_code() {
        Python::attach(|py| {
            let err = RustRiskError::OrderRejected {
                reason: RiskReason::OrderTooLarge {
                    max: 1000.0,
                    actual: 2000.0,
                },
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[OrderRejected]"),
                "expected `[OrderRejected]` in message, got: {s}"
            );
        });
    }

    /// 所有 4 个变体都能成功转 `PyErr`(不 panic)。
    #[test]
    fn to_py_err_handles_all_variants() {
        let variants: Vec<RustRiskError> = vec![
            RustRiskError::CircuitBreakerActive { until: 0 },
            RustRiskError::OrderRejected {
                reason: RiskReason::InsufficientMargin {
                    required: 100.0,
                    available: 50.0,
                },
            },
            RustRiskError::ConfigInvalid("bad".into()),
            RustRiskError::Overflow("f64 overflow".into()),
        ];
        for v in variants {
            // 转 PyErr 不得 panic
            let _py: PyErr = v.into();
        }
    }

    /// `register` 函数签名稳定(编译期断言)。
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
