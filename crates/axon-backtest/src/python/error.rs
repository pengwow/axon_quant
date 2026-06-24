//! `BacktestError` → `PyBacktestError(PyException)` 统一异常转换。
//!
//! 使用 `axon_core::py_exception!` 宏生成异常类 + 错误转换 + 注册函数。
//!
//! 设计:与 `axon-data::python::error` 保持一致 ——
//! - `BacktestError` 继承 builtin `PyException`(不引 `AxonError` 基类,
//!   避免 `axon-backtest` 反向依赖 `axon-python` 造成 cargo 循环);
//! - 用 `From<BacktestErrorKind> for PyErr` 让 `?` 自动转换;
//! - `code` 标签从变体反推,保留 2 个错误源(`MatchingError` /
//!   `MatchingL3Error`)的可识别性。
//!
//! **为什么不引 `BacktestEngine` 自身的错误?**
//! `BacktestEngine::run()` 返回 `RunResult` 而非 `Result`(`engine.rs:194-215`),
//! 不会失败。`step()` 返回 `Option<RunStats>`,失败语义是 `None` 而非 `Result`。
//! 故 `BacktestErrorKind` 只包含底层撮合错误源。

use axon_core::py_exception;

use crate::matching::error::MatchingError;
use crate::matching::l3::error::MatchingL3Error;

/// 内部枚举:统一 2 个 backtest 错误源(Matching / MatchingL3)。
///
/// 未来如需扩展(`StreamError` / `ConfigError` 等),在 enum 中加变体并更新
/// `code()` / `Display` / `From` 即可,**不**改变公开异常类 `BacktestError`。
#[derive(Debug)]
pub enum BacktestErrorKind {
    /// L1/L2 撮合错误
    Matching(MatchingError),
    /// L3 多资产撮合错误
    MatchingL3(MatchingL3Error),
}

impl std::fmt::Display for BacktestErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Matching(e) => write!(f, "{e}"),
            Self::MatchingL3(e) => write!(f, "{e}"),
        }
    }
}

impl From<MatchingError> for BacktestErrorKind {
    fn from(e: MatchingError) -> Self {
        Self::Matching(e)
    }
}

impl From<MatchingL3Error> for BacktestErrorKind {
    fn from(e: MatchingL3Error) -> Self {
        Self::MatchingL3(e)
    }
}

use BacktestErrorKind::*;

py_exception!(
    axon_quant._native.backtest,
    BacktestError,
    BacktestErrorKind,
    {
        Matching(_) => "Matching",
        MatchingL3(_) => "MatchingL3",
    }
);

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;
    use pyo3::prelude::*;

    /// `to_py_err` 反推的 `code` 必须出现在 message 中(`[Code] ...` 形式),
    /// 便于 Python 端 `e.args[1].startswith(f"[{code}]")` 二次校验。
    #[test]
    fn to_py_err_matching_preserves_code_in_message() {
        Python::attach(|py| {
            let err = BacktestErrorKind::Matching(MatchingError::OrderNotFound { order_id: 42 });
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[Matching]"),
                "expected `[Matching]` in message, got: {s}"
            );
        });
    }

    /// `MatchingL3Error` 也能正确转 `PyErr`,code = `"MatchingL3"`。
    #[test]
    fn to_py_err_matching_l3_preserves_code_in_message() {
        Python::attach(|py| {
            let err = BacktestErrorKind::MatchingL3(MatchingL3Error::AuctionNoClearingPrice);
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[MatchingL3]"),
                "expected `[MatchingL3]` in message, got: {s}"
            );
        });
    }

    /// `From<MatchingError> for BacktestErrorKind` 直接转 `PyErr` 链畅通。
    #[test]
    fn from_matching_error_to_py_err() {
        let inner_err = MatchingError::OrderAlreadyFilled;
        let kind: BacktestErrorKind = inner_err.into();
        let py_err: PyErr = kind.into();
        // 不得 panic,Display 含 "已完全成交" 字段
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("已完全成交"), "got: {s}");
        });
    }

    /// `From<MatchingL3Error> for BacktestErrorKind` 链畅通。
    #[test]
    fn from_matching_l3_error_to_py_err() {
        let inner_err = MatchingL3Error::SnapshotFailed("engine crashed".into());
        let kind: BacktestErrorKind = inner_err.into();
        let py_err: PyErr = kind.into();
        Python::attach(|py| {
            let s = py_err.value(py).to_string();
            assert!(s.contains("engine crashed"), "got: {s}");
        });
    }

    /// `register` 函数签名稳定(编译期断言)。
    #[test]
    fn register_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register;
    }
}
