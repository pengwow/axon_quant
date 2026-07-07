//! Python 绑定模块
//!
//! 0.3.0 P0 Batch 4 / T1.13:DeFi 链上交易 Python 绑定
//!
//! 模式与 `axon-oms` / `axon-risk` / `axon-exchange` 一致:
//! - 内部按职责拆分子模块(`error` / `chain` / `types` / `config` /
//!   `evm` / `bridge` / `mev`)
//! - `register_module(parent)` 把全部类**扁平注册**到 `parent`
//!   (`_native.defi`),不嵌套 `add_submodule`(cdylib 单文件扩展
//!   走属性访问即可)

pub mod bridge;
pub mod chain;
pub mod config;
pub mod error;
pub mod evm;
pub mod mev;
pub mod types;

use pyo3::prelude::*;

/// 在父模块(`_native.defi`)下注册全部已实现的子模块。
///
/// 调用方:`crates/axon-python/src/lib.rs::_native` 中先创建
/// `PyModule::new("defi")` 再调 `axon_defi::python::register_module`.
pub fn register_module(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    error::register(parent)?;
    error::register_error_variants(parent)?;
    chain::register(parent)?;
    types::register(parent)?;
    config::register(parent)?;
    evm::register(parent)?;
    bridge::register(parent)?;
    mev::register(parent)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

    /// `register_module` 函数签名稳定:接受 `&Bound<'_, PyModule>`,返回 `PyResult<()>`
    #[test]
    fn register_module_signature() {
        let _f: fn(&Bound<'_, PyModule>) -> PyResult<()> = register_module;
    }

    /// `register_module` 真实执行:把全部 7 个子模块的类挂到父模块。
    #[test]
    fn register_module_attaches_all_classes() {
        Python::attach(|py| {
            let m = PyModule::new(py, "defi").expect("create defi submodule");
            register_module(&m).expect("register_module ok");
            // 抽样验证 7 个核心类都注册成功
            let expected = [
                "DefiError",
                "Chain",
                "EvmConfig",
                "DefiOrder",
                "SwapRoute",
                "RiskCheckResult",
                "UniswapV3Contracts",
                "ProviderConfig",
                "EvmProvider",
                "LocalSigner",
                "Erc20Client",
                "V3Quoter",
                "V3Router",
                "Multicall",
                "BridgeConfig",
                "BridgeManager",
                "MevShareConfig",
                "MevShareClient",
            ];
            for name in expected {
                assert!(
                    m.hasattr(name).unwrap_or(false),
                    "[defi] missing class: {}",
                    name
                );
            }
        });
    }
}
