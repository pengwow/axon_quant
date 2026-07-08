//! 0.3.0 P0 Batch 4 / T1.13:MEV Python 绑定

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_async_runtimes::tokio::future_into_py;

use crate::mev::share::{
    MevShareClient as RustMevShareClient, MevShareConfig as RustMevShareConfig,
};

/// MEV-Share 配置
#[pyclass(name = "MevShareConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyMevShareConfig {
    inner: RustMevShareConfig,
}

#[pymethods]
impl PyMevShareConfig {
    #[staticmethod]
    fn new(rpc_url: String, signing_key: String) -> Self {
        Self {
            inner: RustMevShareConfig::new(rpc_url, signing_key),
        }
    }

    #[staticmethod]
    fn default() -> Self {
        Self {
            inner: RustMevShareConfig::default(),
        }
    }

    #[getter]
    fn rpc_url(&self) -> &str {
        &self.inner.rpc_url
    }

    #[getter]
    fn signing_key(&self) -> &str {
        &self.inner.signing_key
    }

    #[getter]
    fn max_wait_secs(&self) -> u64 {
        self.inner.max_wait_secs
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("rpc_url", &self.inner.rpc_url)?;
        dict.set_item("signing_key", &self.inner.signing_key)?;
        dict.set_item("max_wait_secs", self.inner.max_wait_secs)?;
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!("MevShareConfig(rpc_url='{}')", self.inner.rpc_url)
    }
}

/// MEV-Share 客户端
#[pyclass(name = "MevShareClient", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyMevShareClient {
    inner: RustMevShareClient,
}

#[pymethods]
impl PyMevShareClient {
    #[new]
    fn new(config: PyMevShareConfig) -> Self {
        Self {
            inner: RustMevShareClient::new(config.inner),
        }
    }

    /// 提交 signed tx hex 到 Flashbots relay
    fn submit_transaction<'py>(
        &self,
        py: Python<'py>,
        signed_tx_hex: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let client = self.inner.clone();
        let hex = signed_tx_hex.to_string();
        // 不显式指定第二个泛型,让编译器推断 T = String,
        // future 输出 `Result<String, PyErr>` = `PyResult<String>`,符合 API。
        future_into_py(py, async move {
            client
                .submit_transaction(&hex)
                .await
                .map_err(|e| PyValueError::new_err(format!("{}", e)))
        })
    }

    /// 关联的 RPC 端点
    #[getter]
    fn rpc_url(&self) -> String {
        self.inner.config().rpc_url.clone()
    }

    fn __repr__(&self) -> String {
        format!("MevShareClient(rpc_url='{}')", self.inner.config().rpc_url)
    }
}

/// 注册 MEV 绑定
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyMevShareConfig>()?;
    parent.add_class::<PyMevShareClient>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

    #[test]
    fn mev_share_config_default() {
        Python::attach(|_py| {
            let cfg = PyMevShareConfig::default();
            assert_eq!(cfg.rpc_url(), "https://relay.flashbots.net");
        });
    }

    #[test]
    fn mev_share_client_constructs() {
        Python::attach(|_py| {
            let cfg = PyMevShareConfig::new("http://x".into(), "0xkey".into());
            let _client = PyMevShareClient::new(cfg);
        });
    }
}
