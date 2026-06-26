//! `axon-compliance` Python 绑定模块
//!
//! 把 `axon-compliance` 的合规模块、配置、枚举通过 PyO3 暴露到 Python,
//! 形成 `axon_quant.compliance` 子模块。
//!
//! # 子模块
//!
//! - `error`: `ComplianceError` → `PyComplianceError(PyException)`
//! - `types`: 数据类型(`ComplianceConfig` / `TradeSide` / `OrderType` /
//!   `LiquidityType` / `TradeStatus` / `AuditEventType` / `TradeRecord`)
//! - `module`: `ComplianceModule` 统一合规模块包装 + `load_config_from_toml` 工厂
//!
//! # 与 Stage 1-5 模式一致
//!
//! - `register_module(parent)` 扁平注册到 `_native.compliance`
//! - `ComplianceError` 继承 builtin `PyException` 而非 `AxonError`,避免 cargo 循环
//! - `ComplianceConfig` 用 pyclass + getter 暴露,Rust 内部字段一对一
//! - `record_trade(dict)` 用 dict 协议,内部解析字符串→枚举、UUID、RFC3339

#![cfg(feature = "python")]

pub mod error;
pub mod module;
pub mod types;

use pyo3::prelude::*;

/// 在父模块（`_native.compliance`）下注册全部已实现的子模块。
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(parent)?;
    types::register(parent)?;
    module::register(parent)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_module_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register_module;
    }
}
