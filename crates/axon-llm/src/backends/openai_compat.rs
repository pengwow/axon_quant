//! OpenAI 兼容 LLM backend(支持 DeepSeek、OpenAI、本地推理服务等)
//!
//! 协议:`POST {base_url}/chat/completions`,Bearer auth,JSON body
//!
//! 通过 [`OpenAICompatConfig`] 配置:
//! - `base_url`:API 根,DeepSeek 是 `https://api.deepseek.com/v1`
//! - `api_key`:从 env 读取(不要硬编码)
//! - `model`:模型名,如 `deepseek-chat`
//! - `timeout`:HTTP 超时
//! - `max_tokens` / `temperature`:生成参数
//!
//! 实现 [`LLMBackend`] trait,同时提供 [`stream_complete`](Self::stream_complete) 流式入口。

use super::retry::{BackoffConfig, with_backoff};
use super::streaming::{TokenDelta, sse_bytes_to_deltas};
use crate::backend::{LLMBackend, LLMError, ToolDefinition};
use crate::config::LLMConfig;
use crate::types::{FinishReason, LLMResponse, Message, TokenUsage, ToolCall};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// OpenAI 兼容 backend 配置
#[derive(Debug, Clone)]
pub struct OpenAICompatConfig {
    /// API base URL(末尾不带 `/`)
    pub base_url: String,
    /// API key(Bearer)
    pub api_key: String,
    /// 模型名
    pub model: String,
    /// 单次请求超时
    pub timeout: Duration,
    /// 最大输出 token
    pub max_tokens: u32,
    /// 采样温度
    pub temperature: f32,
    /// 重试配置
    pub backoff: BackoffConfig,
}

impl OpenAICompatConfig {
    /// 从环境变量构造(读取 `DEEPSEEK_API_KEY`,默认 base_url + model)
    pub fn from_env() -> Result<Self, BackendInitError> {
        let api_key = std::env::var("DEEPSEEK_API_KEY")
            .map_err(|_| BackendInitError::MissingEnv("DEEPSEEK_API_KEY"))?;
        Ok(Self {
            base_url: std::env::var("DEEPSEEK_BASE_URL")
                .unwrap_or_else(|_| "https://api.deepseek.com/v1".into()),
            api_key,
            model: std::env::var("DEEPSEEK_MODEL").unwrap_or_else(|_| "deepseek-chat".into()),
            timeout: Duration::from_secs(60),
            max_tokens: 1024,
            temperature: 0.7,
            backoff: BackoffConfig::default(),
        })
    }

    /// 构造一个 deepseek-chat 配置
    pub fn deepseek(api_key: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.deepseek.com/v1".into(),
            api_key: api_key.into(),
            model: "deepseek-chat".into(),
            timeout: Duration::from_secs(60),
            max_tokens: 1024,
            temperature: 0.7,
            backoff: BackoffConfig::default(),
        }
    }

    /// 从统一 `LLMConfig` 构造,选择指定 backend 索引(默认 0)
    ///
    /// 用于支持多 backend(ensemble)场景;索引越界时返回 `BackendInitError`。
    pub fn from_llm_config(cfg: &LLMConfig, index: usize) -> Result<Self, BackendInitError> {
        let b: &crate::config::BackendConfig = cfg
            .backends
            .get(index)
            .ok_or(BackendInitError::MissingEnv("backends[index] not found"))?;
        // RetryConfig 字段单位为毫秒,需转 Duration(BackoffConfig 用 Duration)
        let backoff = BackoffConfig {
            max_retries: cfg.retry.max_retries,
            initial_delay: Duration::from_millis(cfg.retry.initial_backoff_ms),
            max_delay: Duration::from_millis(cfg.retry.max_backoff_ms),
        };
        Ok(Self {
            base_url: b.base_url.clone(),
            api_key: b.api_key.clone(),
            model: b.model.clone(),
            timeout: Duration::from_secs(b.timeout_secs),
            max_tokens: b.max_tokens,
            temperature: b.temperature,
            backoff,
        })
    }
}

/// backend 初始化错误
#[derive(Debug, thiserror::Error)]
pub enum BackendInitError {
    /// 缺少必要环境变量
    #[error("missing env var: {0}")]
    MissingEnv(&'static str),
}

/// OpenAI 兼容 backend
pub struct OpenAICompatBackend {
    config: OpenAICompatConfig,
    client: reqwest::Client,
}

impl OpenAICompatBackend {
    /// 构造 backend
    pub fn new(config: OpenAICompatConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .expect("reqwest client");
        Self { config, client }
    }

    /// 当前配置(只读)
    pub fn config(&self) -> &OpenAICompatConfig {
        &self.config
    }

    /// 构造 HTTP 请求(内部)
    fn build_request_body(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDefinition]>,
    ) -> serde_json::Value {
        let msgs: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                let mut obj = serde_json::json!({
                    "role": m.role.as_str(),
                    "content": m.content,
                });
                if let Some(tcid) = &m.tool_call_id {
                    obj["tool_call_id"] = serde_json::Value::String(tcid.clone());
                }
                if let Some(tcs) = &m.tool_calls {
                    obj["tool_calls"] =
                        serde_json::to_value(tcs).unwrap_or(serde_json::Value::Null);
                }
                obj
            })
            .collect();

        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": msgs,
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
        });
        if let Some(tools) = tools {
            let tool_json: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tool_json);
        }
        body
    }

    /// 流式 chat completion 调用,返回 SSE 解析后的 `TokenDelta` 流
    ///
    /// 调用方负责拼装 `Content` 片段成完整 content + 合并 `ToolCallDelta` 成最终 `ToolCall.arguments`
    ///
    /// ## 告警抑制决策(按 workspace rule #4)
    ///
    /// `#[allow(unused_must_use)]` **必须**保留。原因:
    /// 1. `async_stream::try_stream! { ... }` 宏返回 `AsyncStream<T>`,其实现
    ///    的 `Stream` trait 在 Rust 标准库中被标注为 `#[must_use]`(因为 stream
    ///    必须被 poll 才有意义)
    /// 2. 函数体最后表达式为 `try_stream! { ... }`(已绑到 `let stream = ...`,
    ///    末尾 `stream` 表达式返回),编译器认为"产生了一个 stream 但没 poll"
    /// 3. 实际调用方(如 ReAct agent)会 poll 此 stream;函数返回后 stream 才会
    ///    被消费,所以这个 `must_use` 警告是误报,但需要 `#[allow]` 抑制
    /// 4. 替代方案:`let _ = stream;` 会强制消费 stream,导致 stream 永远不被
    ///    调用方 poll,逻辑错误
    #[allow(unused_must_use)]
    pub fn stream_complete(
        &self,
        messages: &[Message],
    ) -> impl futures_core::Stream<Item = Result<TokenDelta, LLMError>> + 'static {
        // 把需要的所有字段 move 进 stream
        let url = format!("{}/chat/completions", self.config.base_url);
        let api_key = self.config.api_key.clone();
        let model = self.config.model.clone();
        let temperature = self.config.temperature;
        let max_tokens = self.config.max_tokens;
        let _timeout = self.config.timeout; // 占位:可作为 client 构造参数
        let client = self.client.clone();
        let messages = messages.to_vec();

        // 显式 bind stream,让 `try_stream!` 宏产生的 `must_use` 类型变成返回值
        let stream = async_stream::try_stream! {
            use tokio_stream::StreamExt;

            let body = serde_json::json!({
                "model": model,
                "messages": messages,
                "temperature": temperature,
                "max_tokens": max_tokens,
                "stream": true,
            });

            let resp = client
                .post(&url)
                .bearer_auth(&api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| LLMError::Network(e.to_string()))?;

            // 在 if 条件中提取 status(Response 实现了 Copy for status)
            let status = resp.status();
            // 成功路径:resp 转 bytes_stream;错误路径:消费 resp 读 body
            // 关键:`if/else` 中两个分支互斥,编译器知道 resp 只走一条路
            if status.is_success() {
                let byte_stream = resp.bytes_stream();
                let mut delta_stream = std::pin::pin!(sse_bytes_to_deltas(byte_stream));
                while let Some(d) = delta_stream.next().await {
                    yield d?;
                }
            } else {
                let body = resp.text().await.unwrap_or_default();
                Err::<tokio_stream::Once<()>, _>(LLMError::Backend(format!(
                    "status {}: {}",
                    status, body
                )))?;
                // unreachable
            }
        };
        stream
    }
}

#[async_trait]
impl LLMBackend for OpenAICompatBackend {
    async fn complete(&self, messages: &[Message]) -> Result<LLMResponse, LLMError> {
        self.complete_with_tools(messages, &[]).await
    }

    async fn complete_with_tools(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse, LLMError> {
        let url = format!("{}/chat/completions", self.config.base_url);
        let body = self.build_request_body(messages, Some(tools));

        with_backoff(self.config.backoff, || {
            let url = url.clone();
            let client = self.client.clone();
            let api_key = self.config.api_key.clone();
            let body = body.clone();
            async move {
                let resp = client
                    .post(&url)
                    .bearer_auth(&api_key)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| LLMError::Network(e.to_string()))?;
                let status = resp.status();
                if status.as_u16() == 429 {
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok());
                    return Err(LLMError::RateLimited { retry_after });
                }
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(LLMError::Backend(format!("status {}: {}", status, body)));
                }
                let raw: ChatCompletionResp = resp
                    .json()
                    .await
                    .map_err(|e| LLMError::Parse(format!("decode: {e}")))?;
                Ok(raw_to_llm_response(raw))
            }
        })
        .await
    }

    fn context_window_size(&self) -> usize {
        128_000 // DeepSeek 默认
    }
}

/// OpenAI 风格响应
#[derive(Debug, Deserialize)]
struct ChatCompletionResp {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct OpenAIToolCall {
    id: String,
    #[serde(default)]
    #[serde(rename = "type")]
    kind: Option<String>,
    function: OpenAIFunction,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct OpenAIFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize, Default)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: usize,
    #[serde(default)]
    completion_tokens: usize,
    #[serde(default)]
    total_tokens: usize,
}

fn raw_to_llm_response(raw: ChatCompletionResp) -> LLMResponse {
    let choice = raw.choices.into_iter().next();
    let (content, tool_calls, finish_reason) = match choice {
        Some(c) => {
            let tcs: Option<Vec<ToolCall>> = c.message.tool_calls.map(|tcs| {
                tcs.into_iter()
                    .map(|t| ToolCall {
                        id: t.id,
                        function_name: t.function.name,
                        arguments: t.function.arguments,
                    })
                    .collect()
            });
            let fr = match c.finish_reason.as_deref() {
                Some("stop") => FinishReason::Stop,
                Some("length") => FinishReason::Length,
                Some("tool_calls") => FinishReason::ToolCalls,
                Some("content_filter") => FinishReason::ContentFilter,
                _ => FinishReason::Stop,
            };
            (c.message.content, tcs, fr)
        }
        None => (None, None, FinishReason::Stop),
    };
    // 优先使用 server 返回的 total_tokens(可能更准确,因 server 端可能有
    // 内部 tokenization 误差);若 server 没返回(0)则用 prompt+completion 推算
    let usage = raw
        .usage
        .map(|u| {
            let total = if u.total_tokens > 0 {
                u.total_tokens
            } else {
                u.prompt_tokens + u.completion_tokens
            };
            TokenUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: total,
            }
        })
        .unwrap_or_default();
    LLMResponse {
        content,
        tool_calls,
        token_usage: usage,
        finish_reason,
    }
}

// 单元测试用 mock(不需要 HTTP)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendConfig, ExplainConfig, RetryConfig};

    #[test]
    fn config_deepseek() {
        let c = OpenAICompatConfig::deepseek("sk-xxx");
        assert_eq!(c.base_url, "https://api.deepseek.com/v1");
        assert_eq!(c.model, "deepseek-chat");
    }

    #[test]
    fn test_from_llm_config_field_mapping() {
        // 验证 from_llm_config 正确把 LLMConfig 字段映射到 OpenAICompatConfig
        let cfg = LLMConfig {
            backends: vec![BackendConfig {
                name: "primary".into(),
                base_url: "https://x.com/v1".into(),
                api_key: "k".into(),
                model: "m".into(),
                max_tokens: 2048,
                temperature: 0.3,
                timeout_secs: 90,
            }],
            backend: None,
            retry: RetryConfig {
                max_retries: 5,
                initial_backoff_ms: 100,
                max_backoff_ms: 3000,
            },
            explain: ExplainConfig::default(),
        };
        let compat = OpenAICompatConfig::from_llm_config(&cfg, 0).unwrap();
        assert_eq!(compat.base_url, "https://x.com/v1");
        assert_eq!(compat.api_key, "k");
        assert_eq!(compat.model, "m");
        assert_eq!(compat.max_tokens, 2048);
        assert!((compat.temperature - 0.3).abs() < 1e-6);
        assert_eq!(compat.timeout, Duration::from_secs(90));
        // BackoffConfig 使用 Duration;LLMConfig 的 *_backoff_ms 字段为毫秒数
        assert_eq!(compat.backoff.max_retries, 5);
        assert_eq!(compat.backoff.initial_delay, Duration::from_millis(100));
        assert_eq!(compat.backoff.max_delay, Duration::from_millis(3000));
    }

    #[test]
    fn test_from_llm_config_index_out_of_range() {
        // 索引越界应返回 BackendInitError
        let cfg = LLMConfig {
            backends: vec![BackendConfig {
                name: "x".into(),
                base_url: "https://x.com/v1".into(),
                api_key: "k".into(),
                model: "m".into(),
                max_tokens: 1024,
                temperature: 0.7,
                timeout_secs: 60,
            }],
            backend: None,
            retry: RetryConfig::default(),
            explain: ExplainConfig::default(),
        };
        let result = OpenAICompatConfig::from_llm_config(&cfg, 5);
        assert!(matches!(result, Err(BackendInitError::MissingEnv(_))));
    }

    #[test]
    fn build_request_body_basic() {
        let b = OpenAICompatBackend::new(OpenAICompatConfig::deepseek("k"));
        let messages = vec![Message::user("hi")];
        let body = b.build_request_body(&messages, None);
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn build_request_body_with_tools() {
        let b = OpenAICompatBackend::new(OpenAICompatConfig::deepseek("k"));
        let tools = vec![ToolDefinition {
            name: "get_price".into(),
            description: "Get price".into(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let body = b.build_request_body(&[Message::user("x")], Some(&tools));
        assert!(body["tools"].is_array());
        assert_eq!(body["tools"][0]["function"]["name"], "get_price");
    }

    #[test]
    fn raw_to_llm_response_text() {
        let raw = ChatCompletionResp {
            choices: vec![ChatChoice {
                message: ChatMessage {
                    content: Some("Hello".into()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(ChatUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        };
        let r = raw_to_llm_response(raw);
        assert_eq!(r.content.as_deref(), Some("Hello"));
        assert!(r.tool_calls.is_none());
        assert_eq!(r.token_usage.total_tokens, 15);
    }

    #[test]
    fn raw_to_llm_response_tool_calls() {
        let raw = ChatCompletionResp {
            choices: vec![ChatChoice {
                message: ChatMessage {
                    content: None,
                    tool_calls: Some(vec![OpenAIToolCall {
                        id: "call_1".into(),
                        kind: Some("function".into()),
                        function: OpenAIFunction {
                            name: "get_price".into(),
                            arguments: r#"{"symbol":"BTC"}"#.into(),
                        },
                    }]),
                },
                finish_reason: Some("tool_calls".into()),
            }],
            usage: Some(ChatUsage::default()),
        };
        let r = raw_to_llm_response(raw);
        assert!(r.has_tool_calls());
        assert_eq!(r.tool_calls.as_ref().unwrap()[0].function_name, "get_price");
    }
}
