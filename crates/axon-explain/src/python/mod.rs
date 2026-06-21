//! `axon-explain` Python 绑定模块
//!
//! 把 `axon-explain` 的 SHAP 解释器、反事实生成器、报告生成器
//! 通过 PyO3 暴露到 Python，形成 `axon_quant.explain` 子模块。
//!
//! # 子模块
//!
//! - `error`: `ExplainError` → `PyExplainError(PyException)`
//! - `types`: 数据类型（`FeatureContribution`, `Explanation` 等）
//! - `shap`: `KernelSHAP` 解释器
//! - `counterfactual`: `CounterfactualGenerator` 反事实生成器
//! - `report`: `ReportGenerator` 报告生成器
//! - `traits`: `PyModelPredictor` Python 适配器

#![cfg(feature = "python")]

pub mod counterfactual;
pub mod error;
pub mod report;
pub mod shap;
pub mod traits;
pub mod types;

use pyo3::prelude::*;

/// 在父模块（`_native.explain`）下注册全部已实现的子模块。
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(parent)?;
    types::register(parent)?;
    shap::register(parent)?;
    counterfactual::register(parent)?;
    report::register(parent)?;
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
