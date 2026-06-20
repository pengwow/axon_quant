//! `AxonError` 公共异常基类 + 子类工厂。
//!
//! 所有 6 个性能组件的 PyO3 绑定统一继承 `AxonError`:
//! - `axon_quant.AxonError`(基类,统一 `except axon_quant.AxonError as e:`)
//! - `axon_quant.data.DataError`(Stage 1)
//! - `axon_quant.backtest.BacktestError`(Stage 2)
//! - `axon_quant.risk.RiskError`(Stage 3)
//! - `axon_quant.oms.OmsError`(Stage 4)
//! - `axon_quant.exchange.ExchangeError`(Stage 5)
//! - `axon_quant.inference.InferenceError`(Stage 6)
//!
//! **设计原则**:
//! - 不破坏现有 8 个 crate(rl/llm/trading/...)的 PyO3 异常类型;
//!   它们继续 raise `RuntimeError` / `ValueError` / `PermissionError`(已生效),本次不动。
//! - 新增 6 个子类与基类对**老用户透明**——他们继续 `except RuntimeError` 或各自旧异常。
//! - 基类 `AxonError` 继承 `PyException`(不直接继承 `Exception`,以避免与内置异常冲突)。
//! - 子类在各自 crate 的 `python::error` 模块用 `create_exception!` 创建,
//!   第一个参数是 `_native`(统一模块名),保持跨 crate 一致。

use pyo3::exceptions::PyException;
use pyo3::prelude::*;

// AXON Quant 异常基类(继承 `PyException`)。
//
// **为什么继承 `PyException` 而不是 `Exception`?**
// `PyException` 在 Python 中对应 `Exception` 的别名,不会污染标准异常树;
// 同时避免与 builtin `Exception.__init__` 签名耦合(我们只用到 `args[0]` 错误消息)。
//
// 注:这里用 `//` 注释而非 `///`,因 pyo3 0.28 的 `create_exception!` 宏展开
// 不继承 doc comments,否则 rustdoc 会报 `unused_doc_comments` 警告。
pyo3::create_exception!(
    _native,
    AxonError,
    PyException,
    "Base class for all axon_quant specific errors. \
     Catch this to handle any axon_quant error uniformly: \
     `except axon_quant.AxonError as e: ...`"
);

/// 在 `_native` 顶层注册 `AxonError` 基类。
///
/// 调用方:`crates/axon-python/src/lib.rs::axon_python::_native` 的开头
/// 调一次,确保各子模块的 `create_exception!` 在它之后(继承链建立)。
pub fn register_exceptions(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // 用 `py.get_type::<AxonError>()` 拿到 PyType 引用,再 add 到模块
    // (与 axon-data::python::error 的 `register` 写法一致)
    m.py().get_type::<AxonError>();
    m.add("AxonError", m.py().get_type::<AxonError>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `register_exceptions` 函数签名稳定:接受 `&Bound<'_, PyModule>`,返回 `PyResult<()>`。
    /// 这里只验证编译期签名(无运行时调用,因为创建 PyModule 需要 Python 解释器 +
    /// `_native` 模块在 cdylib 加载后才存在)。运行时验证在
    /// `python/tests/test_axon_error.py` 的 Python E2E 测试中(后续 Task 1 收口产物)。
    #[test]
    fn register_exceptions_signature() {
        // 编译期验证:如果 `register_exceptions` 签名变了,这里会编译失败
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register_exceptions;
    }
}
