//! `OmsError` → `PyOmsError(PyException)` 统一异常转换。
//!
//! 设计:与 `axon-risk::python::error` / `axon-backtest::python::error` 保持一致 ——
//! - `OmsError` 继承 builtin `PyException`(不引 `AxonError` 基类,
//!   避免 `axon-oms` 反向依赖 `axon-python` 造成 cargo 循环);
//! - 用 `From<OmsError> for PyErr` 让 `?` 自动转换;
//! - `code` 标签从变体反推,保留所有变体的可识别性;`[Code] message`
//!   形式 message 便于 Python 端 `e.args[1].startswith(f"[{code}]")`
//!   二次校验。
//!
//! Python 端使用示例:
//! ```python
//! try:
//!     mgr.cancel("not-a-uuid")
//! except _native.oms.OmsError as e:
//!     code, message = e.args
//!     if code == "OrderNotFound":
//!         ...
//! ```

use pyo3::exceptions::PyException;
use pyo3::prelude::*;

use crate::error::OmsError as RustOmsError;

// `axon_quant._native.oms.OmsError` —— 继承 builtin `PyException`。
//
// Python 端用 `__module__ = "axon_quant._native.oms"`,但实际 Python 类路径
// 由 `register` 时挂载的位置决定(`_native.oms.OmsError`)。
// (注:`create_exception!` 是宏,上面的 doc 注释必须用 `//` 而非 `///`,
// 否则 rustdoc 报 `unused_doc_comments` 警告 —— 宏展开的 token 不继承 doc。)
pyo3::create_exception!(
    axon_quant._native.oms,
    OmsError,
    PyException,
    "axon-oms specific error. Inherits Exception. \
     `args[0]` is a stable error code (e.g. \"OrderNotFound\" or \"InvalidTransition\"); \
     `args[1]` is a human-readable message in the form `[<code>] <details>`."
);

/// 把 Rust `OmsError` 转 Python 异常。
///
/// 设计:必须从变体反推 `code`,保留每个变体的可识别性。
pub fn to_py_err(err: RustOmsError) -> PyErr {
    // 反推稳定错误码(对应 Python 端 `args[0]`)
    let code = match &err {
        RustOmsError::OrderNotFound(_) => "OrderNotFound",
        RustOmsError::InvalidTransition { .. } => "InvalidTransition",
        RustOmsError::DuplicateIdempotencyKey(_) => "DuplicateIdempotencyKey",
        RustOmsError::AlreadyTerminal(_) => "AlreadyTerminal",
        RustOmsError::ExchangeRejected(_) => "ExchangeRejected",
        RustOmsError::NetworkError(_) => "NetworkError",
        RustOmsError::SerializationError(_) => "SerializationError",
        RustOmsError::RecoveryFailed(_) => "RecoveryFailed",
        RustOmsError::Portfolio(_) => "Portfolio",
    };
    let msg = format!("[{code}] {err}");
    OmsError::new_err((code, msg))
}

impl From<RustOmsError> for PyErr {
    fn from(err: RustOmsError) -> Self {
        to_py_err(err)
    }
}

/// 在 `_native.oms` 子模块下注册 `OmsError` 异常类。
///
/// 调用方:`crates/axon-oms/src/python/mod.rs::register_module`。
///
/// 实现:用 `py.get_type::<OmsError>()` 拿到 PyType,
/// 然后 `parent.add("OmsError", py_type)` 挂到子模块上。
/// 这样不依赖 `axon-python` 的 `_native` Rust 模块,
/// 也避免在 `axon-oms` 中加一个虚拟的 `#[pymodule] fn _native`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    parent.add("OmsError", py.get_type::<OmsError>())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    /// `to_py_err` 反推的 `code` 必须出现在 message 中(`[Code] ...` 形式),
    /// 便于 Python 端 `e.args[1].startswith(f"[{code}]")` 二次校验。
    #[test]
    fn to_py_err_order_not_found_preserves_code() {
        Python::attach(|py| {
            let err = RustOmsError::OrderNotFound("abc-123".into());
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[OrderNotFound]"),
                "expected `[OrderNotFound]` in message, got: {s}"
            );
        });
    }

    /// `InvalidTransition` 变体能正确转 `PyErr`,code = `"InvalidTransition"`。
    #[test]
    fn to_py_err_invalid_transition_preserves_code() {
        Python::attach(|py| {
            let err = RustOmsError::InvalidTransition {
                from: "New".into(),
                to: "Filled".into(),
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[InvalidTransition]"),
                "expected `[InvalidTransition]` in message, got: {s}"
            );
        });
    }

    /// 所有 9 个变体都能成功转 `PyErr`(不 panic)。
    #[test]
    fn to_py_err_handles_all_variants() {
        let variants: Vec<RustOmsError> = vec![
            RustOmsError::OrderNotFound("o1".into()),
            RustOmsError::InvalidTransition {
                from: "A".into(),
                to: "B".into(),
            },
            RustOmsError::DuplicateIdempotencyKey("k1".into()),
            RustOmsError::AlreadyTerminal("o1".into()),
            RustOmsError::ExchangeRejected("exchange down".into()),
            RustOmsError::NetworkError("timeout".into()),
            RustOmsError::SerializationError("bad json".into()),
            RustOmsError::RecoveryFailed("snap mismatch".into()),
            RustOmsError::Portfolio("insufficient cash".into()),
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
