//! `DataError` → `PyDataError(PyException)` 转换。
//!
//! 使用 `axon_core::py_exception!` 宏生成异常类 + 错误转换 + 注册函数。
//! 保持公开 API 不变（`DataError`、`to_py_err`、`register`）。

use axon_core::py_exception;

use crate::error::DataError as RustDataError;
use crate::error::DataError::*;

py_exception!(
    axon_quant._native.data,
    DataError,
    RustDataError,
    {
        SourceNotFound(_) => "SourceNotFound",
        SchemaMismatch { .. } => "SchemaMismatch",
        Network(_) => "Network",
        CorruptData { .. } => "CorruptData",
        RateLimited { .. } => "RateLimited",
        InvalidRequest(_) => "InvalidRequest",
        Io(_) => "Io",
        Internal(_) => "Internal",
        UnsupportedFrequency(_) => "UnsupportedFrequency",
        IpcSchemaMismatch { .. } => "IpcSchemaMismatch",
        #[cfg(feature = "mmap-cache")]
        SharedMemoryCreation(_) => "SharedMemoryCreation",
        #[cfg(feature = "mmap-cache")]
        SharedMemoryMapping(_) => "SharedMemoryMapping",
        #[cfg(feature = "mmap-cache")]
        CacheEntryCorrupted(_) => "CacheEntryCorrupted",
        #[cfg(feature = "mmap-cache")]
        CacheCapacityExceeded { .. } => "CacheCapacityExceeded",
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
    fn to_py_err_preserves_code_in_message() {
        Python::attach(|py| {
            let err = RustDataError::SourceNotFound("mock_source".into());
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(
                s.contains("[SourceNotFound]"),
                "expected `[SourceNotFound]` in message, got: {s}"
            );
        });
    }

    /// 所有 non-mmap 变体都能成功转 `PyErr`(不 panic)。
    /// mmap 变体由另一个 `#[cfg]` 覆盖的测试单独覆盖。
    #[test]
    fn to_py_err_handles_all_default_variants() {
        let variants: Vec<RustDataError> = vec![
            RustDataError::SourceNotFound("x".into()),
            RustDataError::SchemaMismatch {
                expected: "a".into(),
                actual: "b".into(),
            },
            RustDataError::Network("net".into()),
            RustDataError::CorruptData {
                expected: "x".into(),
                actual: "y".into(),
                location: None,
            },
            RustDataError::RateLimited {
                retry_after_ms: 1000,
            },
            RustDataError::InvalidRequest("bad".into()),
            RustDataError::Internal("oops".into()),
            RustDataError::UnsupportedFrequency("1X".into()),
            RustDataError::IpcSchemaMismatch {
                expected: 3,
                actual: 4,
                expected_type: "tick".into(),
            },
        ];
        for v in variants {
            // 转 PyErr 不得 panic
            let _py: PyErr = v.into();
        }
    }

    /// `From<RustDataError> for PyErr` 等价于直接调 `to_py_err`。
    #[test]
    fn from_impl_delegates_to_to_py_err() {
        Python::attach(|py| {
            let a: PyErr = to_py_err(RustDataError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing",
            )));
            let b: PyErr =
                RustDataError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
                    .into();
            assert_eq!(a.value(py).to_string(), b.value(py).to_string());
        });
    }

    /// mmap 变体在 `mmap-cache` feature 启用时也能转(无 panic)。
    #[cfg(feature = "mmap-cache")]
    #[test]
    fn to_py_err_handles_mmap_variants() {
        let variants: Vec<RustDataError> = vec![
            RustDataError::SharedMemoryCreation("shm_open failed".into()),
            RustDataError::SharedMemoryMapping("mmap failed".into()),
            RustDataError::CacheEntryCorrupted("bad checksum".into()),
            RustDataError::CacheCapacityExceeded {
                needed: 1024,
                available: 512,
            },
        ];
        for v in variants {
            let _py: PyErr = v.into();
        }
    }
}
