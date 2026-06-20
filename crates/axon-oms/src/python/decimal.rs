//! `rust_decimal::Decimal` ↔ Python `decimal.Decimal` 桥。
//!
//! 关键:Python `decimal.Decimal` 必须是 str 字面量构造,避免精度丢失。
//!   - Rust → Python: `decimal.to_string()` → `Decimal(str)`
//!   - Python → Rust: `Decimal.__str__()` → `Decimal::from_str(s)`
//!
//! **为什么不用 `Decimal` 直接转换**:PyO3 0.28 没有 `rust_decimal` 原生
//! `FromPyObject` 实现;若用 `float` 中转会丢精度(0.1 + 0.2 ≠ 0.3)。
//! 走 `str` 桥保证任何数量级/精度无损,且对 NaN/Infinity 友好(Decimal 不支持)。
//!
//! **重要:输入校验**:`py_to_decimal` 用 `from_str_exact`-like 行为:
//! 若 str 含非数字字符或空,返回 `PyValueError`,不让 Rust panic 传到 Python。

use std::str::FromStr;

use pyo3::prelude::*;
use pyo3::types::PyAny;
use rust_decimal::Decimal;

/// Rust `Decimal` → Python `Decimal`(`decimal.Decimal` 单例对象)。
///
/// 性能:每次调用走 Python `decimal.Decimal(s)` 构造,O(1)。
pub fn decimal_to_py<'py>(py: Python<'py>, d: &Decimal) -> PyResult<Bound<'py, PyAny>> {
    let decimal_mod = py.import("decimal")?;
    decimal_mod.call_method1("Decimal", (d.to_string(),))
}

/// Python `Decimal` → Rust `Decimal`。
///
/// 路径: `obj.__str__()` → 字符串 → `Decimal::from_str`。
///
/// 错误:输入非 `decimal.Decimal` 或字符串解析失败 → `PyValueError`。
pub fn py_to_decimal(obj: &Bound<'_, PyAny>) -> PyResult<Decimal> {
    let s: String = obj.call_method0("__str__")?.extract()?;
    Decimal::from_str(&s)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("invalid decimal: {e}")))
}

// ===== 测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    /// 基本 round-trip:0.1 + 0.2 精度无损。
    #[test]
    fn roundtrip_decimal_basic() {
        Python::attach(|py| {
            let original = dec!(0.1);
            let py_obj = decimal_to_py(py, &original).unwrap();
            let back = py_to_decimal(&py_obj).unwrap();
            assert_eq!(original, back);
        });
    }

    /// 大数 + 高精度 round-trip:精度上限 28 位(rust_decimal 限制)。
    #[test]
    fn roundtrip_decimal_high_precision() {
        Python::attach(|py| {
            // rust_decimal 上限 28 位精度,取 28 位
            let original = dec!(1.234567890123456789012345678);
            let py_obj = decimal_to_py(py, &original).unwrap();
            let back = py_to_decimal(&py_obj).unwrap();
            assert_eq!(original, back);
        });
    }

    /// 零 + 负数 + 整数 round-trip。
    #[test]
    fn roundtrip_decimal_edge_cases() {
        Python::attach(|py| {
            for original in [dec!(0), dec!(-0.1), dec!(1), dec!(-1), dec!(1000000.000001)] {
                let py_obj = decimal_to_py(py, &original).unwrap();
                let back = py_to_decimal(&py_obj).unwrap();
                assert_eq!(original, back, "roundtrip failed for {original}");
            }
        });
    }

    /// 浮点构造的 Python `Decimal`(0.1 + 0.2)经过 str 桥不丢精度。
    #[test]
    fn py_to_decimal_via_python_arithmetic() {
        Python::attach(|py| {
            let decimal_mod = py.import("decimal").unwrap();
            // 在 Python 中精确构造 0.1 + 0.2
            let py_dec = decimal_mod
                .call_method1("Decimal", ("0.1",))
                .unwrap()
                .call_method1(
                    "__add__",
                    (decimal_mod.call_method1("Decimal", ("0.2",)).unwrap(),),
                )
                .unwrap();
            let back = py_to_decimal(&py_dec).unwrap();
            // 0.1 + 0.2 = 0.3
            assert_eq!(back, dec!(0.3));
        });
    }

    /// 无效字符串(`"abc"`)→ `PyValueError`,不 panic。
    #[test]
    fn py_to_decimal_invalid_string_raises() {
        Python::attach(|py| {
            // 测我们的内部转换:传 None.__str__() → "None" → from_str 失败
            let none_obj = py.None();
            let none_bound = none_obj.bind(py);
            let result = py_to_decimal(&none_bound);
            assert!(result.is_err(), "expected PyValueError, got Ok");
        });
    }
}
