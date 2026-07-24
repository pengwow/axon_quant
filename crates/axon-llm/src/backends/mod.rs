//! LLM 后端实现
//!
//! 提供真实 LLM 提供方的接入,与 [`crate::backend::LLMBackend`] trait 配套使用。
//!
//! 模块:
//! - [`cost`]:token → USD 计价 + 全局定价表
//! - [`retry`]:指数退避 + jitter 重试
//! - [`recording`]:HTTP 录制/回放中间件(vcr 风格,用于 e2e 测试)
//! - [`streaming`]:SSE 流式响应解析(`TokenDelta`)
//! - [`openai_compat`]:OpenAI 兼容 backend(支持 DeepSeek、OpenAI、本地推理服务等)
//!
//! ## Feature flag
//!
//! 本模块由 `backends` feature 控制(默认关闭,避免污染基础构建)。
//! 启用:
//! ```toml
//! axon-llm = { version = "0.1", features = ["backends"] }
//! ```

pub mod cost;
pub mod mock;
pub mod ollama;
pub mod openai_compat;
pub mod recording;
pub mod retry;
pub mod streaming;

// 公共导出
pub use cost::{CostTracker, ModelPricing, pricing_for, register_pricing};
pub use mock::MockBackend;
pub use ollama::{BackendInitError as OllamaInitError, OllamaBackend, OllamaConfig};
pub use openai_compat::{BackendInitError, OpenAICompatBackend, OpenAICompatConfig};
pub use recording::{
    Fixture, Mode, RecordedRequest, RecordedResponse, RecordingLayer, sanitize_request,
    sanitize_response,
};
pub use retry::{BackoffConfig, with_backoff};
pub use streaming::{TokenDelta, parse_sse_body};
