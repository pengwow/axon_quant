//! Python 端交易所配置。
//!
//! 安全注意:`api_secret` **不**暴露到 `__repr__`,避免日志泄漏。
//!
//! 暴露的 pyclass(扁平注册到 `_native.exchange`):
//! - [`PyExchangeId`] — 交易所枚举(Binance / Okx)
//! - [`PyRateLimitConfig`] — REST / WS 速率限制
//! - [`PyReconnectConfig`] — WebSocket 重连 / 熔断配置
//! - [`PyExchangeConfig`] — 完整交易所配置(api_secret 不暴露 getter)

use std::time::Duration;

use pyo3::prelude::*;

use crate::types::{
    ExchangeConfig as RustConfig, ExchangeId as RustId, RateLimitConfig as RustRate,
    ReconnectConfig as RustReconnect,
};

// ─── ExchangeId ─────────────────────────────────────────

/// Python 端交易所枚举。
///
/// 字符串表示沿用 Rust `Display` 实现(小写 `binance` / `okx`)便于
/// JSON 序列化;`__repr__` 沿用 Stage 1-4 一致风格(`ExchangeId.Binance`)。
///
/// `from_py_object`:允许 Python 端将 `ExchangeId.Binance` 作为参数传入
/// `ExchangeConfig.__init__`。
#[pyclass(name = "ExchangeId", eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PyExchangeId {
    Binance,
    Okx,
}

impl From<RustId> for PyExchangeId {
    fn from(id: RustId) -> Self {
        match id {
            RustId::Binance => Self::Binance,
            RustId::Okx => Self::Okx,
        }
    }
}

impl From<PyExchangeId> for RustId {
    fn from(id: PyExchangeId) -> Self {
        match id {
            PyExchangeId::Binance => Self::Binance,
            PyExchangeId::Okx => Self::Okx,
        }
    }
}

#[pymethods]
impl PyExchangeId {
    /// 字符串化:用 Rust `Display` 实现(小写,便于 JSON 序列化)。
    fn __str__(&self) -> &'static str {
        match self {
            Self::Binance => "binance",
            Self::Okx => "okx",
        }
    }

    /// 调试表示:`ExchangeId.Binance` 风格,便于 Python `repr()` 展示。
    fn __repr__(&self) -> String {
        format!("ExchangeId.{}", self.__str__())
    }
}

// ─── RateLimitConfig ────────────────────────────────────

/// Python 端速率限制配置。
///
/// `from_py_object`:允许 Python 端在 `ExchangeConfig(rate_limit=...)`
/// 中作为 keyword 参数传入。
#[pyclass(name = "RateLimitConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyRateLimitConfig {
    pub inner: RustRate,
}

#[pymethods]
impl PyRateLimitConfig {
    /// 构造(默认值与 Rust 默认对齐,符合 Binance 官方限制)。
    #[new]
    #[pyo3(signature = (requests_per_second=10, orders_per_minute=60, ws_messages_per_second=50))]
    fn new(requests_per_second: u32, orders_per_minute: u32, ws_messages_per_second: u32) -> Self {
        Self {
            inner: RustRate {
                requests_per_second,
                orders_per_minute,
                ws_messages_per_second,
            },
        }
    }

    #[getter]
    fn requests_per_second(&self) -> u32 {
        self.inner.requests_per_second
    }

    #[getter]
    fn orders_per_minute(&self) -> u32 {
        self.inner.orders_per_minute
    }

    #[getter]
    fn ws_messages_per_second(&self) -> u32 {
        self.inner.ws_messages_per_second
    }

    fn __repr__(&self) -> String {
        format!(
            "RateLimitConfig(rps={}, opm={}, ws_mps={})",
            self.inner.requests_per_second,
            self.inner.orders_per_minute,
            self.inner.ws_messages_per_second,
        )
    }
}

// ─── ReconnectConfig ────────────────────────────────────

/// Python 端 WebSocket 重连 / 熔断配置。
///
/// `from_py_object`:允许 Python 端在 `ExchangeConfig(reconnect=...)`
/// 中作为 keyword 参数传入。
#[pyclass(name = "ReconnectConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyReconnectConfig {
    pub inner: RustReconnect,
}

#[pymethods]
impl PyReconnectConfig {
    /// 构造(默认值与 Rust 默认对齐)。
    #[new]
    #[pyo3(signature = (
        max_retries=10,
        initial_backoff_ms=500,
        max_backoff_sec=30,
        backoff_multiplier=2.0,
        circuit_breaker_threshold=5,
        circuit_breaker_reset_sec=60,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        max_retries: u32,
        initial_backoff_ms: u64,
        max_backoff_sec: u64,
        backoff_multiplier: f64,
        circuit_breaker_threshold: u32,
        circuit_breaker_reset_sec: u64,
    ) -> Self {
        Self {
            inner: RustReconnect {
                max_retries,
                initial_backoff: Duration::from_millis(initial_backoff_ms),
                max_backoff: Duration::from_secs(max_backoff_sec),
                backoff_multiplier,
                circuit_breaker_threshold,
                circuit_breaker_reset: Duration::from_secs(circuit_breaker_reset_sec),
            },
        }
    }

    #[getter]
    fn max_retries(&self) -> u32 {
        self.inner.max_retries
    }

    #[getter]
    fn initial_backoff_ms(&self) -> u64 {
        self.inner.initial_backoff.as_millis() as u64
    }

    #[getter]
    fn max_backoff_sec(&self) -> u64 {
        self.inner.max_backoff.as_secs()
    }

    #[getter]
    fn backoff_multiplier(&self) -> f64 {
        self.inner.backoff_multiplier
    }

    #[getter]
    fn circuit_breaker_threshold(&self) -> u32 {
        self.inner.circuit_breaker_threshold
    }

    #[getter]
    fn circuit_breaker_reset_sec(&self) -> u64 {
        self.inner.circuit_breaker_reset.as_secs()
    }

    fn __repr__(&self) -> String {
        format!(
            "ReconnectConfig(max_retries={}, init={}ms, max={}s, mult={}, cb_threshold={}, cb_reset={}s)",
            self.inner.max_retries,
            self.inner.initial_backoff.as_millis(),
            self.inner.max_backoff.as_secs(),
            self.inner.backoff_multiplier,
            self.inner.circuit_breaker_threshold,
            self.inner.circuit_breaker_reset.as_secs(),
        )
    }
}

// ─── ExchangeConfig ─────────────────────────────────────

/// 默认 `RateLimitConfig` 值(供 `ExchangeConfig` 缺省)。
fn default_rate_limit() -> RustRate {
    RustRate {
        requests_per_second: 10,
        orders_per_minute: 60,
        ws_messages_per_second: 50,
    }
}

/// 默认 `ReconnectConfig` 值(供 `ExchangeConfig` 缺省)。
fn default_reconnect() -> RustReconnect {
    RustReconnect {
        max_retries: 10,
        initial_backoff: Duration::from_millis(500),
        max_backoff: Duration::from_secs(30),
        backoff_multiplier: 2.0,
        circuit_breaker_threshold: 5,
        circuit_breaker_reset: Duration::from_secs(60),
    }
}

/// 默认 position_endpoint(与 Rust `default_position_endpoint` 一致)。
fn default_position_endpoint() -> String {
    "/fapi/v2/positionRisk".to_string()
}

/// Python 端 `ExchangeConfig` 完整配置。
///
/// **安全:** `__repr__` **不**打印 `api_secret`,避免日志泄漏。
/// `api_secret` 故意不暴露 getter,只走 Rust 内部使用。
///
/// `from_py_object`:允许 Python 端将构造好的 `ExchangeConfig` 实例传入
/// `BinanceAdapter(config)` / `OkxAdapter(config)`。
#[pyclass(name = "ExchangeConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyExchangeConfig {
    pub inner: RustConfig,
}

#[pymethods]
impl PyExchangeConfig {
    #[new]
    #[pyo3(signature = (
        exchange_id,
        api_key,
        api_secret,
        rest_base_url,
        ws_url,
        testnet=true,
        passphrase=None,
        rate_limit=None,
        reconnect=None,
        proxy=None,
        position_endpoint=None,
        fapi_base_url=None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        exchange_id: PyExchangeId,
        api_key: String,
        api_secret: String,
        rest_base_url: String,
        ws_url: String,
        testnet: bool,
        passphrase: Option<String>,
        rate_limit: Option<PyRateLimitConfig>,
        reconnect: Option<PyReconnectConfig>,
        proxy: Option<String>,
        position_endpoint: Option<String>,
        fapi_base_url: Option<String>,
    ) -> Self {
        Self {
            inner: RustConfig {
                exchange_id: exchange_id.into(),
                api_key,
                api_secret,
                passphrase,
                testnet,
                rest_base_url,
                ws_url,
                rate_limit: rate_limit
                    .map(|r| r.inner)
                    .unwrap_or_else(default_rate_limit),
                reconnect: reconnect.map(|r| r.inner).unwrap_or_else(default_reconnect),
                proxy,
                position_endpoint: position_endpoint.unwrap_or_else(default_position_endpoint),
                fapi_base_url,
            },
        }
    }

    #[getter]
    fn exchange_id(&self) -> PyExchangeId {
        self.inner.exchange_id.into()
    }

    #[getter]
    fn api_key(&self) -> String {
        self.inner.api_key.clone()
    }

    // api_secret 故意不暴露 getter,只走 Rust 内部使用
    // (见模块顶部安全说明)。

    #[getter]
    fn passphrase(&self) -> Option<String> {
        self.inner.passphrase.clone()
    }

    #[getter]
    fn testnet(&self) -> bool {
        self.inner.testnet
    }

    #[getter]
    fn rest_base_url(&self) -> String {
        self.inner.rest_base_url.clone()
    }

    #[getter]
    fn ws_url(&self) -> String {
        self.inner.ws_url.clone()
    }

    #[getter]
    fn proxy(&self) -> Option<String> {
        self.inner.proxy.clone()
    }

    #[getter]
    fn position_endpoint(&self) -> String {
        self.inner.position_endpoint.clone()
    }

    #[getter]
    fn fapi_base_url(&self) -> Option<String> {
        self.inner.fapi_base_url.clone()
    }

    /// **安全:`__repr__` 不打印 `api_secret` 和 `api_key`**。
    /// 仅展示交易所 + testnet + URL 摘要,便于调试。
    /// `testnet` 字段用 Python 习惯大写 `True` / `False`(而非 Rust `true` / `false`),
    /// 便于日志阅读与 Stage 1-4 一致。
    fn __repr__(&self) -> String {
        format!(
            "ExchangeConfig({}, testnet={}, rest={}, ws={})",
            self.inner.exchange_id,
            if self.inner.testnet { "True" } else { "False" },
            self.inner.rest_base_url,
            self.inner.ws_url,
        )
    }
}

/// 在父模块下注册所有 config pyclass。
pub fn register(parent: &Bound<'_, PyModule>) -> PyResult<()> {
    parent.add_class::<PyExchangeId>()?;
    parent.add_class::<PyRateLimitConfig>()?;
    parent.add_class::<PyReconnectConfig>()?;
    parent.add_class::<PyExchangeConfig>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认构造(`api_secret` 不暴露)与 `__repr__` 安全断言。
    #[test]
    fn exchange_config_construct_with_defaults() {
        let c = PyExchangeConfig::new(
            PyExchangeId::Binance,
            "k".into(),
            "very_secret_value".into(),
            "https://example.com".into(),
            "wss://example.com/ws".into(),
            true,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert!(c.testnet());
        assert_eq!(c.exchange_id(), PyExchangeId::Binance);
        // `api_key` 可读(用于展示),但 `api_secret` **不**可读
        assert_eq!(c.api_key(), "k");
        // 关键:`__repr__` 不含 `api_secret`,且 `api_key` 也被隐藏(只显示 URL + testnet)
        let r = c.__repr__();
        assert!(
            !r.contains("very_secret_value"),
            "repr leaked api_secret: {r}"
        );
        assert!(!r.contains("k"), "repr leaked api_key: {r}");
        assert!(r.contains("testnet=True"), "repr missing testnet: {r}");
    }

    /// OKX `passphrase` 字段可读(OKX 必须),`testnet` 默认 true。
    #[test]
    fn okx_config_with_passphrase() {
        let c = PyExchangeConfig::new(
            PyExchangeId::Okx,
            "k".into(),
            "s".into(),
            "https://www.okx.com".into(),
            "wss://ws.okx.com:8443/ws/v5/private".into(),
            true,
            Some("pass".into()),
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(c.passphrase(), Some("pass".to_string()));
        assert_eq!(c.exchange_id(), PyExchangeId::Okx);
    }

    /// 自定义 `RateLimitConfig` 透传。
    #[test]
    fn rate_limit_config_custom() {
        let r = PyRateLimitConfig::new(20, 120, 100);
        assert_eq!(r.requests_per_second(), 20);
        assert_eq!(r.orders_per_minute(), 120);
        assert_eq!(r.ws_messages_per_second(), 100);
    }

    /// `ReconnectConfig` 透传,`initial_backoff_ms` / `max_backoff_sec` 单位正确。
    #[test]
    fn reconnect_config_custom() {
        let r = PyReconnectConfig::new(5, 1000, 60, 3.0, 10, 120);
        assert_eq!(r.max_retries(), 5);
        assert_eq!(r.initial_backoff_ms(), 1000);
        assert_eq!(r.max_backoff_sec(), 60);
        assert_eq!(r.circuit_breaker_reset_sec(), 120);
    }

    /// `ExchangeId` enum 字符串表示(沿用 Rust `Display` 小写,便于 JSON)。
    #[test]
    fn exchange_id_str_and_repr() {
        assert_eq!(PyExchangeId::Binance.__str__(), "binance");
        assert_eq!(PyExchangeId::Okx.__str__(), "okx");
        assert_eq!(PyExchangeId::Binance.__repr__(), "ExchangeId.binance");
    }
}
