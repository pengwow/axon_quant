//! Python 绑定工具宏
//!
//! 提供 3 个声明宏，消除 Python 绑定层重复代码：
//! - `py_exception!` — 异常注册 + 错误转换
//! - `parse_py_enum!` — 字符串→枚举转换
//! - `dict_field!` — Dict 字段提取

/// 创建 Python 异常类 + 错误转换 + 注册函数
///
/// # 用法
/// ```ignore
/// py_exception!(
///     axon_quant._native.data,
///     DataError,
///     crate::error::DataError,
///     {
///         SourceNotFound(_) => "SourceNotFound",
///         SchemaMismatch { .. } => "SchemaMismatch",
///         #[cfg(feature = "mmap-cache")]
///         SharedMemoryCreation(_) => "SharedMemoryCreation",
///     }
/// );
/// ```
///
/// 展开生成：
/// - `pyo3::create_exception!` 调用
/// - `to_py_err()` 函数
/// - `From<RustError> for PyErr` 实现
/// - `register()` 函数
///
/// 注意：变体列表用花括号 `{}` 包裹（非方括号 `[]`），以支持 `#[cfg]` 属性。
#[macro_export]
macro_rules! py_exception {
    ($module_path:expr, $exception_name:ident, $rust_error:ty, { $($arms:tt)* }) => {
        pyo3::create_exception!(
            $module_path,
            $exception_name,
            pyo3::exceptions::PyException,
            concat!(
                stringify!($exception_name),
                " - AXON Python exception. `args[0]` is error code, `args[1]` is message."
            )
        );

        /// Convert Rust error to Python exception with error code.
        pub fn to_py_err(err: $rust_error) -> pyo3::PyErr {
            let code = match &err {
                $($arms)*
            };
            let msg = format!("[{code}] {err}");
            $exception_name::new_err((code, msg))
        }

        impl From<$rust_error> for pyo3::PyErr {
            fn from(err: $rust_error) -> Self {
                to_py_err(err)
            }
        }

        /// Register this exception class in the parent Python module.
        pub fn register(parent: &pyo3::Bound<'_, pyo3::types::PyModule>) -> pyo3::PyResult<()> {
            use pyo3::types::PyModuleMethods;
            let py = parent.py();
            parent.add(stringify!($exception_name), py.get_type::<$exception_name>())
        }
    };
}

/// 创建大小写不敏感的字符串→枚举转换函数
///
/// # 用法
/// ```ignore
/// parse_py_enum!(parse_side, Side, [
///     Buy => "buy",
///     Sell => "sell",
/// ]);
/// ```
///
/// 展开生成 `pub fn parse_side(s: &str) -> PyResult<Side>` 函数
#[macro_export]
macro_rules! parse_py_enum {
    ($fn_name:ident, $enum_type:ty, [$($variant:ident => $str:expr),+ $(,)?]) => {
        /// Parse string to enum (case-insensitive).
        pub fn $fn_name(s: &str) -> pyo3::PyResult<$enum_type> {
            match s.to_lowercase().as_str() {
                $(s if s == $str.to_lowercase() => Ok(<$enum_type>::$variant),)+
                _ => Err(pyo3::exceptions::PyValueError::new_err(
                    format!(concat!("invalid ", stringify!($fn_name), ": {}"), s)
                ))
            }
        }
    };
}

/// 从 Python Dict 提取字段，带清晰的错误信息
///
/// # 用法
/// ```ignore
/// let symbol: String = dict_field!(dict, "symbol", String);
/// let price: f64 = dict_field!(dict, "price", f64);
/// ```
///
/// 错误:
/// - 缺字段 → `PyKeyError("missing '<field>'")`
/// - 类型不匹配 → `PyValueError("field '<field>' has wrong type or value")`
#[macro_export]
macro_rules! dict_field {
    ($dict:expr, $key:expr, $type:ty) => {{
        let v = $dict
            .get_item($key)?
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(format!("missing '{}'", $key)))?;
        v.extract::<$type>().map_err(|_e| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "field '{}' has wrong type or value",
                $key
            ))
        })?
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;
    use pyo3::prelude::*;

    // 测试用的简单错误类型
    #[derive(Debug)]
    pub enum TestError {
        NotFound(String),
        Invalid { field: String },
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::NotFound(s) => write!(f, "not found: {s}"),
                Self::Invalid { field } => write!(f, "invalid field: {field}"),
            }
        }
    }

    use TestError::{Invalid, NotFound};

    py_exception!(
        test_module,
        TestPyError,
        TestError,
        {
            NotFound(_) => "NotFound",
            Invalid { .. } => "Invalid",
        }
    );

    #[test]
    fn py_exception_to_py_err_preserves_code() {
        Python::attach(|py| {
            let err = TestError::NotFound("test".into());
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[NotFound]"), "got: {s}");
        });
    }

    #[test]
    fn py_exception_from_impl_works() {
        Python::attach(|py| {
            let err = TestError::Invalid {
                field: "price".into(),
            };
            let py_err: PyErr = err.into();
            let s = py_err.value(py).to_string();
            assert!(s.contains("[Invalid]"), "got: {s}");
        });
    }

    // 测试用的枚举
    #[derive(Debug, PartialEq)]
    enum TestSide {
        Buy,
        Sell,
    }

    parse_py_enum!(parse_test_side, TestSide, [
        Buy => "buy",
        Sell => "sell",
    ]);

    #[test]
    fn parse_py_enum_case_insensitive() {
        assert_eq!(parse_test_side("buy").unwrap(), TestSide::Buy);
        assert_eq!(parse_test_side("BUY").unwrap(), TestSide::Buy);
        assert_eq!(parse_test_side("Buy").unwrap(), TestSide::Buy);
        assert_eq!(parse_test_side("sell").unwrap(), TestSide::Sell);
    }

    #[test]
    fn parse_py_enum_invalid_value() {
        assert!(parse_test_side("hold").is_err());
    }

    #[test]
    fn dict_field_extracts_value() {
        Python::attach(|py| -> PyResult<()> {
            let dict = pyo3::types::PyDict::new(py);
            dict.set_item("symbol", "BTC-USDT").unwrap();
            dict.set_item("price", 100.5).unwrap();

            let symbol: String = dict_field!(dict, "symbol", String);
            assert_eq!(symbol, "BTC-USDT");

            let price: f64 = dict_field!(dict, "price", f64);
            assert!((price - 100.5).abs() < f64::EPSILON);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn dict_field_missing_key_raises() {
        Python::attach(|py| {
            let dict = pyo3::types::PyDict::new(py);
            let result: PyResult<String> = (|| Ok(dict_field!(dict, "missing", String)))();
            assert!(result.is_err());
        });
    }
}
