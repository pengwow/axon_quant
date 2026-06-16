//! LLM SSE 流式响应解析
//!
//! 提供 [`TokenDelta`] 枚举和 [`OpenAICompatBackend::stream_complete`](super::OpenAICompatBackend::stream_complete) 方法,
//! 用于按 token / 工具调用增量处理 LLM 响应。
//!
//! 调用方负责把 `Content` 片段拼装成完整 content + 合并 `ToolCallDelta` 为最终 `ToolCall.arguments`。

use crate::backend::LLMError;
use async_stream::try_stream;
use serde::Deserialize;

/// 单个 token / 工具调用增量
#[derive(Debug, Clone, PartialEq)]
pub enum TokenDelta {
    /// 文本片段(累加得到完整 content)
    Content(String),
    /// 工具调用开始(携带 id + 函数名,参数在 `ToolCallDelta` 后续追加)
    ToolCallStart {
        /// 工具调用 ID
        id: String,
        /// 函数名
        name: String,
    },
    /// 工具调用参数增量(JSON 字符串片段,需累加后再 parse)
    ToolCallDelta {
        /// 工具调用 ID
        id: String,
        /// 参数 JSON 字符串片段
        arguments_delta: String,
    },
    /// 流结束(携带 finish_reason 字符串,语义同 `map_finish_reason`)
    Done {
        /// 原始 `finish_reason`(可能为空)
        finish_reason: String,
    },
}

/// ChatCompletionChunk 的最小子集(只解析 streaming 用得到的字段)
#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    /// choices 列表
    choices: Vec<ChunkChoice>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    /// delta 字段(可能为空)
    #[serde(default)]
    delta: ChunkDelta,
    /// finish_reason(仅最后一个 chunk 携带)
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkDelta {
    /// 文本片段
    #[serde(default)]
    content: Option<String>,
    /// 工具调用增量
    #[serde(default)]
    tool_calls: Option<Vec<ChunkToolCall>>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkToolCall {
    /// 工具调用 ID(仅首 chunk 携带)
    #[serde(default)]
    id: Option<String>,
    /// function 子结构
    #[serde(default)]
    function: Option<ChunkFunction>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkFunction {
    /// 函数名(仅首 chunk 携带)
    #[serde(default)]
    name: Option<String>,
    /// 参数片段(后续 chunk 携带)
    #[serde(default)]
    arguments: String,
}

/// 解析 SSE 文本(单行)为 `TokenDelta` 列表(单行可能产生 0/1/N 个 delta)
fn parse_sse_line(line: &str) -> Vec<TokenDelta> {
    let line = line.trim();
    if line.is_empty() {
        return vec![];
    }
    let data = match line.strip_prefix("data: ") {
        Some(d) => d,
        None => return vec![],
    };
    if data == "[DONE]" {
        return vec![TokenDelta::Done {
            finish_reason: "stop".into(),
        }];
    }
    match serde_json::from_str::<ChatCompletionChunk>(data) {
        Ok(chunk) => {
            let mut out = Vec::new();
            for choice in chunk.choices {
                if let Some(content) = choice.delta.content
                    && !content.is_empty()
                {
                    out.push(TokenDelta::Content(content));
                }
                if let Some(tool_calls) = choice.delta.tool_calls {
                    for tc in tool_calls {
                        // ToolCallStart:同时有 id 和 function.name
                        if let (Some(id), Some(func)) = (&tc.id, tc.function.as_ref())
                            && let Some(name) = &func.name
                        {
                            out.push(TokenDelta::ToolCallStart {
                                id: id.clone(),
                                name: name.clone(),
                            });
                        }
                        // ToolCallDelta:有 arguments 片段
                        if let Some(func) = &tc.function
                            && !func.arguments.is_empty()
                        {
                            let id = tc.id.clone().unwrap_or_default();
                            out.push(TokenDelta::ToolCallDelta {
                                id,
                                arguments_delta: func.arguments.clone(),
                            });
                        }
                    }
                }
                if let Some(fr) = choice.finish_reason {
                    out.push(TokenDelta::Done { finish_reason: fr });
                }
            }
            out
        }
        Err(_e) => vec![], // 忽略单个 chunk 解析错误
    }
}

/// 把整个 SSE body 解析为 TokenDelta 列表(辅助函数,供 [`super::OpenAICompatBackend::stream_complete`] 内部使用)
pub fn parse_sse_body(body: &str) -> Vec<TokenDelta> {
    let mut out = Vec::new();
    for line in body.split('\n') {
        out.extend(parse_sse_line(line));
    }
    out
}

// ─── 真正的 stream_complete(在 OpenAICompatBackend 上) ───

/// 把 SSE 字节流转 TokenDelta 流(由 `OpenAICompatBackend::stream_complete` 内部使用)
///
/// 把所需字段 move 进 stream(避免借用 self 生命周期问题)
pub fn sse_bytes_to_deltas(
    body_stream: impl tokio_stream::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
) -> impl tokio_stream::Stream<Item = Result<TokenDelta, LLMError>> + Send + 'static {
    use tokio_stream::StreamExt;

    try_stream! {
        let mut stream = std::pin::pin!(body_stream);
        let mut buffer = String::new();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| LLMError::Network(e.to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            // 按行处理 SSE
            while let Some(idx) = buffer.find('\n') {
                let line: String = buffer.drain(..=idx).collect();
                for d in parse_sse_line(&line) {
                    yield d;
                }
            }
        }
        // 处理最后一行(可能没有换行符)
        if !buffer.trim().is_empty() {
            for d in parse_sse_line(&buffer) {
                yield d;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_content_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let deltas = parse_sse_line(line);
        assert_eq!(deltas, vec![TokenDelta::Content("Hello".into())]);
    }

    #[test]
    fn parse_done_marker() {
        let line = "data: [DONE]";
        let deltas = parse_sse_line(line);
        assert_eq!(
            deltas,
            vec![TokenDelta::Done {
                finish_reason: "stop".into()
            }]
        );
    }

    #[test]
    fn parse_tool_call_start() {
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"call_1","function":{"name":"get_price"}}]}}]}"#;
        let deltas = parse_sse_line(line);
        assert!(
            deltas
                .iter()
                .any(|d| matches!(d, TokenDelta::ToolCallStart { id, name }
            if id == "call_1" && name == "get_price"))
        );
    }

    #[test]
    fn parse_tool_call_delta() {
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{\"sym\""}}]}}]}"#;
        let deltas = parse_sse_line(line);
        assert!(deltas.iter().any(
            |d| matches!(d, TokenDelta::ToolCallDelta { arguments_delta, .. }
                if arguments_delta == r#"{"sym""#)
        ));
    }

    #[test]
    fn parse_sse_body_combines_chunks() {
        let body = r#"data: {"choices":[{"delta":{"content":"Hi "}}]}

data: {"choices":[{"delta":{"content":"there"}}]}

data: [DONE]
"#;
        let deltas = parse_sse_body(body);
        let contents: Vec<String> = deltas
            .iter()
            .filter_map(|d| match d {
                TokenDelta::Content(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(contents, vec!["Hi ", "there"]);
        assert!(deltas.iter().any(|d| matches!(d, TokenDelta::Done { .. })));
    }
}
