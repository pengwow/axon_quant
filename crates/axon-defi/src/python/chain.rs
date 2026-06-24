//! Chain 枚举 Python 绑定

use pyo3::prelude::*;

use crate::evm::chain::Chain;

/// 支持的 EVM 链
#[pyclass(name = "Chain", eq, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyChain {
    /// Ethereum 主网
    Ethereum,
    /// Arbitrum One
    Arbitrum,
    /// Optimism
    Optimism,
    /// Polygon
    Polygon,
}

#[pymethods]
impl PyChain {
    #[getter]
    fn chain_id(&self) -> u64 {
        let chain: Chain = (*self).into();
        chain.chain_id()
    }

    #[getter]
    fn name(&self) -> &'static str {
        let chain: Chain = (*self).into();
        chain.name()
    }

    /// 从链 ID 创建
    #[staticmethod]
    fn from_chain_id(chain_id: u64) -> PyResult<Self> {
        let chain = Chain::from_chain_id(chain_id).map_err(PyErr::from)?;
        Ok(chain.into())
    }

    fn __str__(&self) -> &'static str {
        let chain: Chain = (*self).into();
        chain.name()
    }

    fn __repr__(&self) -> String {
        format!("Chain.{}", self.__str__())
    }
}

impl From<Chain> for PyChain {
    fn from(c: Chain) -> Self {
        match c {
            Chain::Ethereum => Self::Ethereum,
            Chain::Arbitrum => Self::Arbitrum,
            Chain::Optimism => Self::Optimism,
            Chain::Polygon => Self::Polygon,
        }
    }
}

impl From<PyChain> for Chain {
    fn from(c: PyChain) -> Self {
        match c {
            PyChain::Ethereum => Self::Ethereum,
            PyChain::Arbitrum => Self::Arbitrum,
            PyChain::Optimism => Self::Optimism,
            PyChain::Polygon => Self::Polygon,
        }
    }
}

/// 注册枚举到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyChain>()?;
    Ok(())
}
