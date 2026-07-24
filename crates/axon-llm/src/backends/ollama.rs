//! Ollama 本地推理 backend
//!
//! Ollama 是一个本地 LLM 运行时，通过 OpenAI 兼容协议暴露 API。
//! 协议:`POST {base_url}/chat/completions`,无认证(本地运行)。
//!
//! 通过 [`OllamaConfig`] 配置:
//! - `base_url`:Ollama API 根,默认 `http://localhost:11434/v1`
//! - `model`:模型名,如 `llama3`
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

/// Ollama backend 配置
#[derive(Debug, Clone)]
pub struct OllamaConfig {
    pub base_url: String,
    pub model: String,
    pub timeout: Duration,
    pub max_tokens: u32,
    pub temperature: f32,
    pub backoff: BackoffConfig,
}

impl OllamaConfig {
    pub fn from_env() -> Result<Self, BackendInitError> {
        Ok(Self {
            base_url: std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434/v1".into()),
            model: std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3".into()),
            timeout: Duration::from_secs(60),
            max_tokens: 1024,
            temperature: 0.7,
            backoff: BackoffConfig::default(),
        })
    }

    pub fn from_llm_config(cfg: &LLMConfig, index: usize) -> Result<Self, BackendInitError> {
        let b: &crate::config::BackendConfig = cfg
            .backends
            .get(index)
            .ok_or(BackendInitError::MissingEnv("backends[index] not found"))?;
        let backoff = BackoffConfig {
            max_retries: cfg.retry.max_retries,
            initial_delay: Duration::from_millis(cfg.retry.initial_backoff_ms),
            max_delay: Duration::from_millis(cfg.retry.max_backoff_ms),
        };
        Ok(Self {
            base_url: b.base_url.clone(),
            model: b.model.clone(),
            timeout: Duration::from_secs(b.timeout_secs),
            max_tokens: b.max_tokens,
            temperature: b.temperature,
            backoff,
        })
    }

    pub fn llama3() -> Self {
        Self {
            base_url: "http://localhost:11434/v1".into(),
            model: "llama3".into(),
            timeout: Duration::from_secs(60),
            max_tokens: 1024,
            temperature: 0.7,
            backoff: BackoffConfig::default(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = temperature;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackendInitError {
    #[error("missing env var: {0}")]
    MissingEnv(&'static str),
}

/// Ollama backend
pub struct OllamaBackend {
    config: OllamaConfig,
    client: reqwest::Client,
}

impl OllamaBackend {
    pub fn new(config: OllamaConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .expect("reqwest client");
        Self { config, client }
    }

    pub fn config(&self) -> &OllamaConfig {
        &self.config
    }

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

    #[allow(unused_must_use)]
    pub fn stream_complete(
        &self,
        messages: &[Message],
    ) -> impl futures_core::Stream<Item = Result<TokenDelta, LLMError>> + 'static {
        let url = format!("{}/chat/completions", self.config.base_url);
        let model = self.config.model.clone();
        let temperature = self.config.temperature;
        let max_tokens = self.config.max_tokens;
        let client = self.client.clone();
        let messages = messages.to_vec();

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
                .json(&body)
                .send()
                .await
                .map_err(|e| LLMError::Network(e.to_string()))?;

            let status = resp.status();
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
            }
        };
        stream
    }
}

#[async_trait]
impl LLMBackend for OllamaBackend {
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
            let body = body.clone();
            async move {
                let resp = client
                    .post(&url)
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
        128_000
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendConfig, ExplainConfig, RetryConfig};

    #[test]
    fn config_llama3_default() {
        let c = OllamaConfig::llama3();
        assert_eq!(c.base_url, "http://localhost:11434/v1");
        assert_eq!(c.model, "llama3");
        assert_eq!(c.max_tokens, 1024);
        assert!((c.temperature - 0.7).abs() < 1e-6);
    }

    #[test]
    fn config_builder_methods() {
        let c = OllamaConfig::llama3()
            .with_model("mistral")
            .with_max_tokens(2048)
            .with_temperature(0.5);
        assert_eq!(c.model, "mistral");
        assert_eq!(c.max_tokens, 2048);
        assert!((c.temperature - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_from_llm_config_field_mapping() {
        let cfg = LLMConfig {
            backends: vec![BackendConfig {
                name: "ollama".into(),
                base_url: "http://localhost:11434/v1".into(),
                api_key: "".into(),
                model: "llama3".into(),
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
        let ollama = OllamaConfig::from_llm_config(&cfg, 0).unwrap();
        assert_eq!(ollama.base_url, "http://localhost:11434/v1");
        assert_eq!(ollama.model, "llama3");
        assert_eq!(ollama.max_tokens, 2048);
        assert!((ollama.temperature - 0.3).abs() < 1e-6);
        assert_eq!(ollama.timeout, Duration::from_secs(90));
        assert_eq!(ollama.backoff.max_retries, 5);
        assert_eq!(ollama.backoff.initial_delay, Duration::from_millis(100));
        assert_eq!(ollama.backoff.max_delay, Duration::from_millis(3000));
    }

    #[test]
    fn test_from_llm_config_index_out_of_range() {
        let cfg = LLMConfig {
            backends: vec![BackendConfig {
                name: "ollama".into(),
                base_url: "http://localhost:11434/v1".into(),
                api_key: "".into(),
                model: "llama3".into(),
                max_tokens: 1024,
                temperature: 0.7,
                timeout_secs: 60,
            }],
            backend: None,
            retry: RetryConfig::default(),
            explain: ExplainConfig::default(),
        };
        let result = OllamaConfig::from_llm_config(&cfg, 5);
        assert!(matches!(result, Err(BackendInitError::MissingEnv(_))));
    }

    #[test]
    fn build_request_body_basic() {
        let b = OllamaBackend::new(OllamaConfig::llama3());
        let messages = vec![Message::user("hi")];
        let body = b.build_request_body(&messages, None);
        assert_eq!(body["model"], "llama3");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn build_request_body_with_tools() {
        let b = OllamaBackend::new(OllamaConfig::llama3());
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