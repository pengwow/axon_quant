//! # axon-exchange
//!
//! 交易所对接：REST + WebSocket，Binance/OKX 适配器。
//!
//! ## 核心功能
//!
//! - **ExchangeAdapter trait**：统一的交易所接口
//! - **WebSocket 管理**：指数退避重连 + 熔断器
//! - **令牌桶限流**：符合交易所 API 限制
//! - **订单生命周期**：状态机管理、崩溃恢复
//! - **多交易所**：Binance、OKX 适配器
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! use axon_exchange::{ExchangeConfig, ExchangeId, Symbol, RateLimitConfig, ReconnectConfig};
//! use std::time::Duration;
//!
//! // 配置交易所
//! let config = ExchangeConfig {
//!     exchange_id: ExchangeId::Binance,
//!     api_key: "test_key".into(),
//!     api_secret: "test_secret".into(),
//!     passphrase: None,
//!     testnet: true,
//!     rest_base_url: "https://testnet.binance.vision".into(),
//!     ws_url: "wss://testnet.binance.vision/ws".into(),
//!     rate_limit: RateLimitConfig {
//!         requests_per_second: 10,
//!         orders_per_minute: 60,
//!         ws_messages_per_second: 50,
//!     },
//!     reconnect: ReconnectConfig {
//!         max_retries: 10,
//!         initial_backoff: Duration::from_millis(500),
//!         max_backoff: Duration::from_secs(30),
//!         backoff_multiplier: 2.0,
//!         circuit_breaker_threshold: 5,
//!         circuit_breaker_reset: Duration::from_secs(60),
//!     },
//!     proxy: None,
//!     position_endpoint: "/fapi/v2/positionRisk".into(),
//! };
//! ```
//!
//! ## 支持的交易所
//!
//! | 交易所 | REST | WebSocket | 测试网 |
//! |--------|------|-----------|--------|
//! | Binance | ✅ | ✅ | ✅ |
//! | OKX | ✅ | ✅ | ✅ |
//!
//! ## 性能目标
//!
//! | 操作 | 目标 |
//! |------|------|
//! | 下单延迟 | < 50ms |
//! | 行情接收 | < 5ms |
//! | 重连恢复 | < 5s |

pub mod adapters;
pub mod error;
pub mod lifecycle;
pub mod rate_limiter;
pub mod sign;
pub mod traits;
pub mod types;
pub mod ws;

pub use error::ExchangeError;
pub use lifecycle::{OrderLifecycleManager, OrderRecord, TrackedOrder};
pub use rate_limiter::TokenBucketRateLimiter;
pub use sign::{binance as sign_binance, okx as sign_okx};
pub use traits::ExchangeAdapter;
pub use types::*;
pub use ws::manager::WebSocketManager;

use std::time::Duration;

use reqwest::Client;

/// 构造 HTTP 客户端，支持代理配置。
///
/// 优先使用 `config.proxy`，否则 reqwest 自动读取系统环境变量。
/// 统一供各交易所适配器复用，避免重复实现。
pub fn build_http_client(config: &ExchangeConfig) -> Client {
    let mut builder = Client::builder().timeout(Duration::from_secs(10));
    if let Some(proxy_url) = &config.proxy
        && let Ok(proxy) = reqwest::Proxy::all(proxy_url)
    {
        builder = builder.proxy(proxy);
    }
    builder.build().expect("failed to create HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> ExchangeConfig {
        ExchangeConfig {
            exchange_id: ExchangeId::Binance,
            api_key: "k".into(),
            api_secret: "s".into(),
            passphrase: None,
            testnet: true,
            rest_base_url: "https://example.com".into(),
            ws_url: "wss://example.com/ws".into(),
            rate_limit: RateLimitConfig {
                requests_per_second: 10,
                orders_per_minute: 60,
                ws_messages_per_second: 50,
            },
            reconnect: ReconnectConfig {
                max_retries: 1,
                initial_backoff: Duration::from_millis(100),
                max_backoff: Duration::from_secs(1),
                backoff_multiplier: 2.0,
                circuit_breaker_threshold: 1,
                circuit_breaker_reset: Duration::from_secs(1),
            },
            proxy: None,
            position_endpoint: "/fapi/v2/positionRisk".into(),
            fapi_base_url: None,
        }
    }

    #[test]
    fn test_build_http_client_without_proxy() {
        // 无代理配置时应正常返回 Client
        let client = build_http_client(&base_config());
        // reqwest::Client 内部状态不直接暴露，至少验证能构造成功
        let _ = client.get("https://example.com");
    }

    #[test]
    fn test_build_http_client_with_invalid_proxy_ignored() {
        // 无效代理 URL 应被静默忽略，构造仍能成功（实现内部用 `if let Ok`）
        let cfg = ExchangeConfig {
            proxy: Some("not a valid proxy url".into()),
            ..base_config()
        };
        let client = build_http_client(&cfg);
        let _ = client.get("https://example.com");
    }
}
