//! 0.3.0 P0 Batch 4 / T1.13:EVM + DEX Python 绑定

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_async_runtimes::tokio::future_into_py;

use crate::dex::v3_quoter::V3Quoter as RustV3Quoter;
use crate::dex::v3_router::V3Router as RustV3Router;
use crate::evm::erc20::Erc20Client as RustErc20Client;
use crate::evm::multicall::Multicall as RustMulticall;
use crate::evm::provider::{EvmProvider as RustEvmProvider, ProviderConfig as RustProviderConfig};
use crate::evm::signer::LocalSigner as RustLocalSigner;

// ============================================================
// ProviderConfig
// ============================================================

/// Provider 配置
#[pyclass(name = "ProviderConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyProviderConfig {
    inner: RustProviderConfig,
}

#[pymethods]
impl PyProviderConfig {
    /// 按链 + RPC URL 快速构造
    #[staticmethod]
    fn for_chain(chain: super::chain::PyChain, rpc_url: String) -> Self {
        Self {
            inner: RustProviderConfig::for_chain(chain.into(), rpc_url),
        }
    }

    #[getter]
    fn rpc_url(&self) -> &str {
        &self.inner.rpc_url
    }

    #[getter]
    fn timeout_ms(&self) -> u64 {
        self.inner.timeout_ms
    }

    #[getter]
    fn max_retries(&self) -> u32 {
        self.inner.max_retries
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("rpc_url", &self.inner.rpc_url)?;
        dict.set_item("timeout_ms", self.inner.timeout_ms)?;
        dict.set_item("max_retries", self.inner.max_retries)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!("ProviderConfig(rpc_url='{}')", self.inner.rpc_url)
    }
}

impl From<RustProviderConfig> for PyProviderConfig {
    fn from(c: RustProviderConfig) -> Self {
        Self { inner: c }
    }
}

// ============================================================
// EvmProvider
// ============================================================

/// EVM Provider(真链 RPC 客户端)
#[pyclass(name = "EvmProvider", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyEvmProvider {
    pub(crate) inner: RustEvmProvider,
}

#[pymethods]
impl PyEvmProvider {
    #[new]
    fn new(config: PyProviderConfig) -> Self {
        Self {
            inner: RustEvmProvider::new(config.inner),
        }
    }

    /// 查链 ID(走真 RPC)
    fn chain_id<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let provider = self.inner.clone();
        future_into_py(py, async move {
            #[cfg(feature = "evm")]
            {
                provider
                    .chain_id()
                    .await
                    .map_err(|e| PyValueError::new_err(format!("{}", e)))
            }
            #[cfg(not(feature = "evm"))]
            {
                Err::<u64, _>(PyValueError::new_err("evm feature not enabled"))
            }
        })
    }

    /// 查最新 block number(走真 RPC)
    fn block_number<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let provider = self.inner.clone();
        future_into_py(py, async move {
            #[cfg(feature = "evm")]
            {
                provider
                    .block_number()
                    .await
                    .map_err(|e| PyValueError::new_err(format!("{}", e)))
            }
            #[cfg(not(feature = "evm"))]
            {
                Err::<u64, _>(PyValueError::new_err("evm feature not enabled"))
            }
        })
    }

    #[getter]
    fn rpc_url(&self) -> String {
        self.inner.config().rpc_url.clone()
    }

    fn __repr__(&self) -> String {
        format!("EvmProvider(rpc_url='{}')", self.inner.config().rpc_url)
    }
}

// ============================================================
// LocalSigner
// ============================================================

/// 本地私钥签名器
#[pyclass(name = "LocalSigner", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyLocalSigner {
    pub(crate) inner: RustLocalSigner,
}

#[pymethods]
impl PyLocalSigner {
    /// 从 hex 私钥构造(0x 前缀 + 64 hex chars)
    #[staticmethod]
    fn from_hex(hex: &str, chain: super::chain::PyChain) -> PyResult<Self> {
        RustLocalSigner::from_hex(hex, chain.into())
            .map(|s| Self { inner: s })
            .map_err(|e| PyValueError::new_err(format!("{}", e)))
    }

    /// 签名地址
    #[getter]
    fn address(&self) -> String {
        #[cfg(feature = "evm")]
        {
            format!("{:?}", self.inner.address())
        }
        #[cfg(not(feature = "evm"))]
        {
            String::new()
        }
    }

    /// 分配并返回下一个 nonce
    #[getter]
    fn next_nonce(&self) -> u64 {
        self.inner.next_nonce()
    }

    fn __repr__(&self) -> String {
        #[cfg(feature = "evm")]
        {
            format!("LocalSigner(address={:?})", self.inner.address())
        }
        #[cfg(not(feature = "evm"))]
        {
            "LocalSigner(<evm feature off>)".to_string()
        }
    }
}

// ============================================================
// Erc20Client
// ============================================================

/// Token 元信息(对应 Rust `TokenInfo`,Option 字段在 Python 端允许 None)
#[pyclass(name = "TokenInfo", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyTokenInfo {
    inner: crate::evm::erc20::TokenInfo,
}

#[pymethods]
impl PyTokenInfo {
    #[getter]
    fn address(&self) -> String {
        self.inner.address.clone()
    }

    #[getter]
    fn decimals(&self) -> Option<u8> {
        self.inner.decimals
    }

    #[getter]
    fn symbol(&self) -> Option<String> {
        self.inner.symbol.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "TokenInfo(address='{}', symbol={:?}, decimals={:?})",
            self.inner.address, self.inner.symbol, self.inner.decimals
        )
    }
}

/// ERC-20 客户端
#[pyclass(name = "Erc20Client", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyErc20Client {
    inner: RustErc20Client,
}

#[pymethods]
impl PyErc20Client {
    #[new]
    fn new(address: &str, provider: PyEvmProvider) -> Self {
        Self {
            inner: RustErc20Client::new(address, provider.inner),
        }
    }

    /// token 元信息(地址 / decimals / symbol 缓存)
    #[getter]
    fn info(&self) -> PyTokenInfo {
        PyTokenInfo {
            inner: self.inner.info().clone(),
        }
    }

    /// token decimals(走真 RPC,已知名预设)
    fn decimals<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        future_into_py(py, async move {
            client
                .decimals()
                .await
                .map_err(|e| PyValueError::new_err(format!("{}", e)))
        })
    }

    /// token symbol(走真 RPC,已知名预设)
    fn symbol<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        future_into_py(py, async move {
            client
                .symbol()
                .await
                .map_err(|e| PyValueError::new_err(format!("{}", e)))
        })
    }

    /// 查询某地址 token 余额
    fn balance_of<'py>(&self, py: Python<'py>, holder: &str) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        let holder = holder.to_string();
        future_into_py(py, async move {
            #[cfg(feature = "evm")]
            {
                // 内部 `balance_of(&str)` 已校验地址格式并 parse,
                // 这里不重复 parse,直接传 &str 即可
                let bal = client
                    .balance_of(&holder)
                    .await
                    .map_err(|e| PyValueError::new_err(format!("{}", e)))?;
                Ok(bal.to_string())
            }
            #[cfg(not(feature = "evm"))]
            {
                Err::<String, _>(PyValueError::new_err("evm feature not enabled"))
            }
        })
    }

    fn __repr__(&self) -> String {
        format!("Erc20Client(address='{}')", self.inner.info().address)
    }
}

// ============================================================
// V3Quoter
// ============================================================

/// V3 Quoter 客户端
#[pyclass(name = "V3Quoter", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyV3Quoter {
    inner: RustV3Quoter,
}

#[pymethods]
impl PyV3Quoter {
    #[new]
    fn new(provider: PyEvmProvider, chain: super::chain::PyChain) -> Self {
        Self {
            inner: RustV3Quoter::new(provider.inner, chain.into()),
        }
    }

    /// 真链 quote(token_in, token_out, amount_in, fee) → amount_out (string)
    fn quote_exact_input_single<'py>(
        &self,
        py: Python<'py>,
        token_in: &str,
        token_out: &str,
        amount_in: &str,
        fee: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let quoter = self.inner.clone();
        let ti = token_in.to_string();
        let to = token_out.to_string();
        let ai = amount_in.to_string();
        future_into_py(py, async move {
            #[cfg(feature = "evm")]
            {
                use alloy::primitives::{Address, U256};
                let ti: Address = ti
                    .parse()
                    .map_err(|e| PyValueError::new_err(format!("invalid token_in: {}", e)))?;
                let to: Address = to
                    .parse()
                    .map_err(|e| PyValueError::new_err(format!("invalid token_out: {}", e)))?;
                let ai: U256 = ai
                    .parse()
                    .map_err(|e| PyValueError::new_err(format!("invalid amount_in: {}", e)))?;
                let res = quoter
                    .quote_exact_input_single(ti, to, ai, fee, U256::ZERO)
                    .await
                    .map_err(|e| PyValueError::new_err(format!("{}", e)))?;
                Ok(res.amount_out.to_string())
            }
            #[cfg(not(feature = "evm"))]
            {
                Err::<String, _>(PyValueError::new_err("evm feature not enabled"))
            }
        })
    }

    fn __repr__(&self) -> String {
        format!("V3Quoter(quoter='{}')", self.inner.address())
    }

    /// QuoterV2 合约地址
    #[getter]
    fn address(&self) -> String {
        self.inner.address()
    }
}

// ============================================================
// V3Router
// ============================================================

/// V3 SwapRouter02 客户端
#[pyclass(name = "V3Router", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyV3Router {
    inner: RustV3Router,
}

#[pymethods]
impl PyV3Router {
    #[new]
    fn new(provider: PyEvmProvider, chain: super::chain::PyChain) -> Self {
        Self {
            inner: RustV3Router::new(provider.inner, chain.into()),
        }
    }

    #[getter]
    fn address(&self) -> String {
        self.inner.address()
    }

    fn __repr__(&self) -> String {
        format!("V3Router(router='{}')", self.inner.address())
    }
}

// ============================================================
// Multicall
// ============================================================

/// Multicall3 批量查询
#[pyclass(name = "Multicall", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyMulticall {
    inner: RustMulticall,
}

#[pymethods]
impl PyMulticall {
    #[new]
    fn new(provider: PyEvmProvider, chain: super::chain::PyChain) -> Self {
        Self {
            inner: RustMulticall::new(provider.inner, chain.into()),
        }
    }

    /// 批量查 balanceOf(token, [holder1, holder2, ...]) -> [bal1, bal2, ...] (string)
    fn balance_of_batch<'py>(
        &self,
        py: Python<'py>,
        token: &str,
        holders: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mc = self.inner.clone();
        let token = token.to_string();
        future_into_py(py, async move {
            #[cfg(feature = "evm")]
            {
                use alloy::primitives::Address;
                let holders: Result<Vec<Address>, _> = holders
                    .iter()
                    .map(|s| {
                        s.parse::<Address>()
                            .map_err(|e| PyValueError::new_err(format!("invalid holder {}: {}", s, e)))
                    })
                    .collect();
                let holders = holders?;
                let bals = mc
                    .balance_of_batch(&token, &holders)
                    .await
                    .map_err(|e| PyValueError::new_err(format!("{}", e)))?;
                Ok(bals
                    .into_iter()
                    .map(|b| b.to_string())
                    .collect::<Vec<_>>())
            }
            #[cfg(not(feature = "evm"))]
            {
                Err::<Vec<String>, _>(PyValueError::new_err("evm feature not enabled"))
            }
        })
    }

    fn __repr__(&self) -> String {
        format!("Multicall(address='{}')", self.inner.address())
    }

    /// Multicall3 合约地址
    #[getter]
    fn address(&self) -> String {
        self.inner.address()
    }
}

// ============================================================
// 注册
// ============================================================

/// 注册 EVM + DEX 绑定到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyProviderConfig>()?;
    parent.add_class::<PyEvmProvider>()?;
    parent.add_class::<PyLocalSigner>()?;
    parent.add_class::<PyTokenInfo>()?;
    parent.add_class::<PyErc20Client>()?;
    parent.add_class::<PyV3Quoter>()?;
    parent.add_class::<PyV3Router>()?;
    parent.add_class::<PyMulticall>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

    #[test]
    fn provider_config_pyobject_construction() {
        Python::attach(|_py| {
            // 验证 PyProviderConfig 能构造
            let chain = super::super::chain::PyChain::Ethereum;
            let cfg = PyProviderConfig::for_chain(chain, "http://x".into());
            assert_eq!(cfg.rpc_url(), "http://x");
        });
    }

    #[test]
    fn evm_provider_constructs_from_config() {
        Python::attach(|_py| {
            let chain = super::super::chain::PyChain::Ethereum;
            let cfg = PyProviderConfig::for_chain(chain, "http://x".into());
            let p = PyEvmProvider::new(cfg);
            assert!(p.__repr__().contains("http://x"));
        });
    }
}
