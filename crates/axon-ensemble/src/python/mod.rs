//! `axon-ensemble` Python 绑定模块
//!
//! 把 `axon-ensemble` 的投票策略、集成管理器、堆叠/动态加权集成
//! 通过 PyO3 暴露到 Python，形成 `axon_quant.ensemble` 子模块。

#![cfg(feature = "python")]

pub mod error;
pub mod manager;
pub mod stacking;
pub mod traits;
pub mod types;
pub mod voting;

use pyo3::prelude::*;

/// 在父模块（`_native.ensemble`）下注册全部已实现的子模块。
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(parent)?;
    types::register(parent)?;
    voting::register(parent)?;
    manager::register(parent)?;
    stacking::register(parent)?;
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
