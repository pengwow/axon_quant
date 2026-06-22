//! Python 类型定义

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::types::{DefiOrder, EvmConfig, RiskCheckResult, SwapRoute};

/// EVM 链配置
#[pyclass(name = "EvmConfig", skip_from_py_object)]
#[derive(Debug, Clone)]
pub struct PyEvmConfig {
    inner: EvmConfig,
}

#[pymethods]
impl PyEvmConfig {
    /// 创建新的 EVM 配置
    #[new]
    fn new(chain_id: u64, rpc_url: String, private_key: String) -> Self {
        Self {
            inner: EvmConfig::new(chain_id, rpc_url, private_key),
        }
    }

    /// 设置 1inch API Key
    #[staticmethod]
    fn with_oneinch_api_key(
        chain_id: u64,
        rpc_url: String,
        private_key: String,
        key: String,
    ) -> Self {
        Self {
            inner: EvmConfig::new(chain_id, rpc_url, private_key).with_oneinch_api_key(key),
        }
    }

    /// 设置 Flashbots RPC
    #[staticmethod]
    fn with_flashbots_rpc(
        chain_id: u64,
        rpc_url: String,
        private_key: String,
        rpc: String,
    ) -> Self {
        Self {
            inner: EvmConfig::new(chain_id, rpc_url, private_key).with_flashbots_rpc(rpc),
        }
    }

    #[getter]
    fn chain_id(&self) -> u64 {
        self.inner.chain_id
    }

    #[getter]
    fn rpc_url(&self) -> &str {
        &self.inner.rpc_url
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("chain_id", self.inner.chain_id)?;
        dict.set_item("rpc_url", &self.inner.rpc_url)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!(
            "EvmConfig(chain_id={}, rpc_url='{}')",
            self.inner.chain_id, self.inner.rpc_url
        )
    }
}

impl PyEvmConfig {
    /// 获取内部引用
    pub fn inner(&self) -> &EvmConfig {
        &self.inner
    }
}

/// DeFi 订单
#[pyclass(name = "DefiOrder", skip_from_py_object)]
#[derive(Debug, Clone)]
pub struct PyDefiOrder {
    inner: DefiOrder,
}

#[pymethods]
impl PyDefiOrder {
    /// 创建新的 DeFi 订单
    #[new]
    fn new(token: String, amount: String, amount_usd: f64) -> Self {
        Self {
            inner: DefiOrder::new(token, amount, amount_usd),
        }
    }

    /// 设置滑点
    #[staticmethod]
    fn with_slippage(token: String, amount: String, amount_usd: f64, slippage: f64) -> Self {
        Self {
            inner: DefiOrder::new(token, amount, amount_usd).with_slippage(slippage),
        }
    }

    /// 设置目标地址
    #[staticmethod]
    fn with_to(token: String, amount: String, amount_usd: f64, to: String) -> Self {
        Self {
            inner: DefiOrder::new(token, amount, amount_usd).with_to(to),
        }
    }

    #[getter]
    fn token(&self) -> &str {
        &self.inner.token
    }

    #[getter]
    fn amount(&self) -> &str {
        &self.inner.amount
    }

    #[getter]
    fn amount_usd(&self) -> f64 {
        self.inner.amount_usd
    }

    #[getter]
    fn slippage(&self) -> f64 {
        self.inner.slippage
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("token", &self.inner.token)?;
        dict.set_item("amount", &self.inner.amount)?;
        dict.set_item("amount_usd", self.inner.amount_usd)?;
        dict.set_item("slippage", self.inner.slippage)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!(
            "DefiOrder(token='{}', amount='{}', amount_usd={:.2})",
            self.inner.token, self.inner.amount, self.inner.amount_usd
        )
    }
}

impl PyDefiOrder {
    /// 获取内部引用
    pub fn inner(&self) -> &DefiOrder {
        &self.inner
    }
}

/// 交易路由
#[pyclass(name = "SwapRoute", skip_from_py_object)]
#[derive(Debug, Clone)]
pub struct PySwapRoute {
    inner: SwapRoute,
}

#[pymethods]
impl PySwapRoute {
    #[getter]
    fn token_in(&self) -> &str {
        &self.inner.token_in
    }

    #[getter]
    fn token_out(&self) -> &str {
        &self.inner.token_out
    }

    #[getter]
    fn fee(&self) -> u32 {
        self.inner.fee
    }

    #[getter]
    fn amount_in(&self) -> &str {
        &self.inner.amount_in
    }

    #[getter]
    fn amount_out(&self) -> &str {
        &self.inner.amount_out
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("token_in", &self.inner.token_in)?;
        dict.set_item("token_out", &self.inner.token_out)?;
        dict.set_item("fee", self.inner.fee)?;
        dict.set_item("amount_in", &self.inner.amount_in)?;
        dict.set_item("amount_out", &self.inner.amount_out)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!(
            "SwapRoute({} -> {}, fee={})",
            self.inner.token_in, self.inner.token_out, self.inner.fee
        )
    }
}

impl PySwapRoute {
    /// 从 Rust 类型创建
    pub fn from_rust(r: SwapRoute) -> Self {
        Self { inner: r }
    }
}

/// 风控检查结果
#[pyclass(name = "RiskCheckResult", skip_from_py_object)]
#[derive(Debug, Clone)]
pub struct PyRiskCheckResult {
    inner: RiskCheckResult,
}

#[pymethods]
impl PyRiskCheckResult {
    #[getter]
    fn approved(&self) -> bool {
        self.inner.approved
    }

    #[getter]
    fn reason(&self) -> Option<&str> {
        self.inner.reason.as_deref()
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("approved", self.inner.approved)?;
        dict.set_item("reason", &self.inner.reason)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!("RiskCheckResult(approved={})", self.inner.approved)
    }
}

impl PyRiskCheckResult {
    /// 从 Rust 类型创建
    pub fn from_rust(r: RiskCheckResult) -> Self {
        Self { inner: r }
    }
}

/// 注册类型到 Python 模块
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyEvmConfig>()?;
    parent.add_class::<PyDefiOrder>()?;
    parent.add_class::<PySwapRoute>()?;
    parent.add_class::<PyRiskCheckResult>()?;
    Ok(())
}
