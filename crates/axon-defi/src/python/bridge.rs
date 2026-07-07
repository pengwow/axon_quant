//! 0.3.0 P0 Batch 4 / T1.13:Bridge Python 绑定

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_async_runtimes::tokio::future_into_py;

use crate::bridge::layerzero::{
    BridgeConfig as RustBridgeConfig, BridgeManager as RustBridgeManager,
    MessagingParamsInput as RustMessagingParamsInput,
};
use crate::evm::chain::Chain as RustChain;

/// Bridge 配置
#[pyclass(name = "BridgeConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyBridgeConfig {
    inner: RustBridgeConfig,
}

#[pymethods]
impl PyBridgeConfig {
    /// 默认配置(支持 4 链)
    #[staticmethod]
    fn default() -> Self {
        Self {
            inner: RustBridgeConfig::default(),
        }
    }

    #[getter]
    fn endpoint(&self) -> &str {
        &self.inner.endpoint
    }

    #[getter]
    fn supported_chains(&self) -> Vec<u64> {
        self.inner.supported_chains.clone()
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("endpoint", &self.inner.endpoint)?;
        dict.set_item("supported_chains", &self.inner.supported_chains)?;
        dict.set_item("default_slippage", self.inner.default_slippage)?;
        dict.set_item("timeout_secs", self.inner.timeout_secs)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!(
            "BridgeConfig(endpoint='{}', chains={:?})",
            self.inner.endpoint, self.inner.supported_chains
        )
    }
}

/// Bridge 管理器
#[pyclass(name = "BridgeManager", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyBridgeManager {
    inner: RustBridgeManager,
}

#[pymethods]
impl PyBridgeManager {
    #[new]
    fn new(config: PyBridgeConfig) -> Self {
        Self {
            inner: RustBridgeManager::new(config.inner),
        }
    }

    #[getter]
    fn config(&self) -> PyBridgeConfig {
        PyBridgeConfig {
            inner: self.inner.config().clone(),
        }
    }

    /// 验证目标链支持
    fn is_supported(&self, chain: super::chain::PyChain) -> bool {
        self.inner.is_supported(&chain.into())
    }

    /// 估算跨链 native fee(走真 EndpointV2.quote)
    fn estimate_fee<'py>(
        &self,
        py: Python<'py>,
        provider: super::evm::PyEvmProvider,
        params: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mgr = self.inner.clone();
        let provider = provider.inner;
        let parsed = parse_messaging_params(params)?;
        future_into_py(py, async move {
            #[cfg(feature = "evm")]
            {
                mgr.estimate_fee(&provider, &parsed)
                    .await
                    .map(|fee| fee.to_string())
                    .map_err(|e| PyValueError::new_err(format!("{}", e)))
            }
            #[cfg(not(feature = "evm"))]
            {
                Err::<String, _>(PyValueError::new_err("evm feature not enabled"))
            }
        })
    }

    /// 发起跨链转账(走真 EndpointV2.send)
    ///
    /// `params` 字典字段(全部必填):
    /// - `dst_eid`:int — 目标链 LayerZero EID
    /// - `receiver`:str — 接收者地址(0x 前缀 20 字节地址,内部右 pad 32 字节)
    /// - `message`:bytes/str(0x hex)— 跨链消息(本模块不内联 ABI 编码,
    ///   调用方按 OApp/OFT 协议自行序列化)
    /// - `options`:bytes/str(0x hex)— LayerZero executor options,默认 `b""`
    /// - `pay_in_lz_token`:bool — false = native 付 fee
    ///
    /// 返回 dict 含 `tx_hash` / `block_number` / `status` / `gas_used`
    fn bridge_tokens<'py>(
        &self,
        py: Python<'py>,
        signer: super::evm::PyLocalSigner,
        provider: super::evm::PyEvmProvider,
        dst_chain: super::chain::PyChain,
        params: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mgr = self.inner.clone();
        let signer = signer.inner;
        let provider = provider.inner;
        let dst: RustChain = dst_chain.into();
        let parsed = parse_messaging_params(params)?;
        future_into_py(py, async move {
            #[cfg(feature = "evm")]
            {
                let receipt = mgr
                    .bridge_tokens(&signer, &provider, &dst, &parsed)
                    .await
                    .map_err(|e| PyValueError::new_err(format!("{}", e)))?;
                Ok(receipt_to_dict(&receipt))
            }
            #[cfg(not(feature = "evm"))]
            {
                Err::<(String, u64, bool, u64), _>(PyValueError::new_err(
                    "evm feature not enabled",
                ))
            }
        })
    }

    fn __repr__(&self) -> String {
        format!("BridgeManager(endpoint='{}')", self.inner.config().endpoint)
    }
}

/// 把 Python dict 解析成 `MessagingParamsInput`
fn parse_messaging_params(dict: &Bound<'_, PyDict>) -> PyResult<RustMessagingParamsInput> {
    let dst_eid: u32 = dict
        .get_item("dst_eid")?
        .ok_or_else(|| PyValueError::new_err("params.dst_eid is required"))?
        .extract()
        .map_err(|e| PyValueError::new_err(format!("invalid dst_eid: {}", e)))?;

    let receiver_str: String = dict
        .get_item("receiver")?
        .ok_or_else(|| PyValueError::new_err("params.receiver is required"))?
        .extract()
        .map_err(|e| PyValueError::new_err(format!("invalid receiver: {}", e)))?;
    let receiver_bytes32 = address_to_bytes32(&receiver_str)?;

    let message: Vec<u8> = extract_bytes(dict, "message")?;
    let options: Vec<u8> = extract_bytes(dict, "options").unwrap_or_default();
    let pay_in_lz_token: bool = dict
        .get_item("pay_in_lz_token")?
        .map(|v| v.extract())
        .transpose()
        .map_err(|e| PyValueError::new_err(format!("invalid pay_in_lz_token: {}", e)))?
        .unwrap_or(false);

    Ok(RustMessagingParamsInput {
        dst_eid,
        receiver_bytes32,
        message,
        options,
        pay_in_lz_token,
    })
}

/// hex string("0x" + 40 hex) → [u8; 32](左 12 字节 0x00 + 20 字节地址)
fn address_to_bytes32(addr: &str) -> PyResult<[u8; 32]> {
    let s = addr.strip_prefix("0x").unwrap_or(addr);
    if s.len() != 40 {
        return Err(PyValueError::new_err(format!(
            "address must be 20 bytes (40 hex chars), got {}",
            s.len()
        )));
    }
    let bytes = alloy::primitives::hex::decode(s)
        .map_err(|e| PyValueError::new_err(format!("invalid hex: {}", e)))?;
    let mut out = [0u8; 32];
    out[12..32].copy_from_slice(&bytes);
    Ok(out)
}

/// 从 dict 抽 bytes(支持 `bytes` / `bytearray` / "0x hex" str)
fn extract_bytes(dict: &Bound<'_, PyDict>, key: &str) -> PyResult<Vec<u8>> {
    let v = dict
        .get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("params.{} is required", key)))?;
    // 先试 bytes
    if let Ok(b) = v.extract::<Vec<u8>>() {
        return Ok(b);
    }
    // 再试 str(0x hex)
    if let Ok(s) = v.extract::<String>() {
        let stripped = s.strip_prefix("0x").unwrap_or(&s);
        return alloy::primitives::hex::decode(stripped)
            .map_err(|e| PyValueError::new_err(format!("{}: invalid hex: {}", key, e)));
    }
    Err(PyValueError::new_err(format!(
        "{} must be bytes or 0x-prefixed hex string",
        key
    )))
}

/// `TransactionReceipt` → Python 元组(tx_hash, block_number, status, gas_used)
fn receipt_to_dict(
    receipt: &alloy::rpc::types::TransactionReceipt,
) -> (String, u64, bool, u64) {
    (
        format!("{:?}", receipt.transaction_hash),
        receipt.block_number.unwrap_or(0),
        receipt.status(),
        receipt.gas_used,
    )
}

/// 注册 Bridge 绑定
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyBridgeConfig>()?;
    parent.add_class::<PyBridgeManager>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

    #[test]
    fn bridge_config_default_constructs() {
        Python::attach(|_py| {
            let cfg = PyBridgeConfig::default();
            assert!(!cfg.endpoint().is_empty());
            assert!(cfg.supported_chains().contains(&1)); // Ethereum
        });
    }

    #[test]
    fn bridge_manager_supports_chains() {
        Python::attach(|_py| {
            let mgr = PyBridgeManager::new(PyBridgeConfig::default());
            let eth = super::super::chain::PyChain::Ethereum;
            let arb = super::super::chain::PyChain::Arbitrum;
            assert!(mgr.is_supported(eth));
            assert!(mgr.is_supported(arb));
        });
    }
}
