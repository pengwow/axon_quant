//! `DataError` → `PyDataError(PyException)` 转换。
//!
//! 设计:不依赖 `axon-python`(`AxonError` 基类在那里),
//! 而是把 `DataError` 直接挂到 builtin `PyException` 下;
//! Python 端用 `except Exception` 统一捕获,或用 `except _native.data.DataError`
//! 精确捕获(避免跨 crate cargo 依赖,见设计文档 §3.1.6)。
//!
//! 关键:用 `From<DataError> for PyErr` 让 `?` 自动转换;
//! `code` 标签从变体反推,保留 11+ 个变体的可识别性,便于 Python 端
//! `except _native.data.DataError as e: e.args[0]` 拿到 code,
//! `e.args[1]` 拿到 `[Code] message` 形式的展示串。

use pyo3::exceptions::PyException;
use pyo3::prelude::*;

use crate::error::DataError as RustDataError;

// `axon_quant._native.data.DataError` —— 继承 builtin `PyException`。
//
// Python 端用 `__module__ = "axon_quant._native.data"`,但实际 Python 类路径
// 由 `register` 时挂载的位置决定(`_native.data.DataError`)。
//
// 使用示例:
// ```python
// try:
//     svc.load(req)
// except _native.data.DataError as e:
//     code, message = e.args
//     if code == "SourceNotFound":
//         ...
// ```
pyo3::create_exception!(
    axon_quant._native.data,
    DataError,
    PyException,
    "axon-data specific error. Inherits Exception. \
     `args[0]` is a stable error code (e.g. \"SourceNotFound\"); \
     `args[1]` is a human-readable message in the form `[<code>] <details>`."
);

/// 把 Rust `DataError` 转 Python 异常。
///
/// 设计:用 `From<DataError> for PyErr` 让 `?` 自动转换;
/// 关键:必须从变体反推 `code`,保留每个变体的可识别性。
pub fn to_py_err(err: RustDataError) -> PyErr {
    // 反推稳定错误码(对应 Python 端 `args[0]`)
    let code = match &err {
        RustDataError::SourceNotFound(_) => "SourceNotFound",
        RustDataError::SchemaMismatch { .. } => "SchemaMismatch",
        RustDataError::Network(_) => "Network",
        RustDataError::CorruptData { .. } => "CorruptData",
        RustDataError::RateLimited { .. } => "RateLimited",
        RustDataError::InvalidRequest(_) => "InvalidRequest",
        RustDataError::Io(_) => "Io",
        RustDataError::Internal(_) => "Internal",
        RustDataError::UnsupportedFrequency(_) => "UnsupportedFrequency",
        RustDataError::IpcSchemaMismatch { .. } => "IpcSchemaMismatch",
        // mmap-cache feature 仅在 `axon-data` 启用对应 feature 时编译
        #[cfg(feature = "mmap-cache")]
        RustDataError::SharedMemoryCreation(_) => "SharedMemoryCreation",
        #[cfg(feature = "mmap-cache")]
        RustDataError::SharedMemoryMapping(_) => "SharedMemoryMapping",
        #[cfg(feature = "mmap-cache")]
        RustDataError::CacheEntryCorrupted(_) => "CacheEntryCorrupted",
        #[cfg(feature = "mmap-cache")]
        RustDataError::CacheCapacityExceeded { .. } => "CacheCapacityExceeded",
    };
    // 用 Display 信息构造 message;`code` 通过 args[0] 暴露,`[code] message` 通过 args[1] 暴露
    let msg = format!("[{code}] {err}");
    DataError::new_err((code, msg))
}

impl From<RustDataError> for PyErr {
    fn from(err: RustDataError) -> Self {
        to_py_err(err)
    }
}

/// 在 `_native.data` 子模块下注册 `DataError` 异常类。
///
/// 调用方:`crates/axon-data/src/python/mod.rs::register_module`。
///
/// 实现:用 `py.get_type::<DataError>()` 拿到 PyType,
/// 然后 `parent.add("DataError", py_type)` 挂到子模块上。
/// 这样不依赖 axon-python 的 `_native` Rust 模块,
/// 也避免在 axon-data 中加一个虚拟的 `#[pymodule] fn _native`。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = parent.py();
    parent.add("DataError", py.get_type::<DataError>())
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

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
