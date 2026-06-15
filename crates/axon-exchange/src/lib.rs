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
pub mod traits;
pub mod types;
pub mod ws;

pub use error::ExchangeError;
pub use lifecycle::{OrderLifecycleManager, OrderRecord, TrackedOrder};
pub use rate_limiter::TokenBucketRateLimiter;
pub use traits::ExchangeAdapter;
pub use types::*;
pub use ws::manager::WebSocketManager;
