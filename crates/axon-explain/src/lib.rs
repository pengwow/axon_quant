//! AXON 可解释性引擎
//!
//! SHAP 特征归因 + 反事实解释 + 决策报告生成。

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod counterfactual;
pub mod error;
#[cfg(feature = "python")]
pub mod python;
pub mod report;
pub mod shap;
pub mod traits;
pub mod types;
