//! 配置类 Python 绑定

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::dex::uniswap::UniswapV3Contracts;
use crate::evm::chain::Chain;

/// Uniswap V3 合约配置
#[pyclass(name = "UniswapV3Contracts", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyUniswapV3Contracts {
    inner: UniswapV3Contracts,
}

#[pymethods]
impl PyUniswapV3Contracts {
    /// 获取指定链的合约地址
    #[staticmethod]
    fn for_chain(chain: super::chain::PyChain) -> Self {
        let rust_chain: Chain = chain.into();
        Self {
            inner: UniswapV3Contracts::for_chain(&rust_chain),
        }
    }

    #[getter]
    fn factory(&self) -> &str {
        &self.inner.factory
    }

    #[getter]
    fn router(&self) -> &str {
        &self.inner.router
    }

    #[getter]
    fn position_manager(&self) -> &str {
        &self.inner.position_manager
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("factory", &self.inner.factory)?;
        dict.set_item("router", &self.inner.router)?;
        dict.set_item("position_manager", &self.inner.position_manager)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!("UniswapV3Contracts(factory='{}')", self.inner.factory)
    }
}

/// 注册配置类到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyUniswapV3Contracts>()?;
    Ok(())
}
