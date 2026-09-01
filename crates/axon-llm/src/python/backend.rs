//! PyO3 绑定:将 `OpenAICompatBackend` 暴露给 Python
//!
//! `PyLLMBackend` 持有内部 backend + tokio runtime,使 Python 端可以同步
//! 调用 `chat()` / `chat_with_tools()`,内部把 async 调用桥到 sync。
//!
//! ## Python 用法
//!
//! ```python
//! from axon_quant._native.llm import make_backend, LLMMessage
//!
//! backend = make_backend({
//!     "backends": [{
//!         "base_url": "https://x.com/v1",
//!         "api_key": "sk-xxx",
//!         "model": "gpt-4o-mini",
//!     }],
//! })
//! resp = backend.chat([LLMMessage("user", "Hi!")])
//! # 或者直接传 dict(更贴近 OpenAI 原生消息结构)
//! resp = backend.chat([{"role": "user", "content": "Hi!"}])
//! print(resp["content"])
//! ```

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;

use crate::backend::{LLMBackend, LLMError, ToolDefinition};
use crate::backends::{OpenAICompatBackend, TokenDelta};
use crate::types::Message;

use super::helpers::pythonize;

/// Python 端可见的 LLM backend 包装
///
/// 内部持有一个 `OpenAICompatBackend` + 一个 `tokio::runtime::Runtime`,
/// 通过 `block_on` 桥接 async → sync,使 Python 端能直接同步调用 `chat()`。
#[pyclass(name = "LLMBackend")]
#[derive(Clone)]
pub struct PyLLMBackend {
    /// 内部 backend(用 Mutex 包装以便未来支持可重入)
    pub(crate) inner: Arc<Mutex<OpenAICompatBackend>>,
    /// 独占的 tokio runtime
    pub(crate) runtime: Arc<tokio::runtime::Runtime>,
}

#[pymethods]
impl PyLLMBackend {
    /// 同步 chat:Python list[`LLMMessage` | dict] → Rust `Message[]` → LLM → Python dict
    ///
    /// 每条消息可以是 `LLMMessage` 实例,也可以是包含 `role`/`content` 的 dict
    /// (允许可选字段 `tool_call_id` / `tool_calls`,后者为 JSON 字符串)。
    fn chat<'py>(
        &self,
        py: Python<'py>,
        messages: Vec<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let mut msgs: Vec<Message> = Vec::with_capacity(messages.len());
        for m in &messages {
            msgs.push(parse_py_message(m)?);
        }

        let backend = self.inner.clone();
        let resp = self
            .runtime
            .block_on(async move { backend.lock().await.complete(&msgs).await })
            .map_err(map_err)?;

        let dict = PyDict::new(py);
        dict.set_item("content", resp.content.unwrap_or_default())?;
        dict.set_item(
            "reasoning_content",
            resp.reasoning_content.unwrap_or_default(),
        )?;
        dict.set_item("finish_reason", format!("{:?}", resp.finish_reason))?;
        dict.set_item("prompt_tokens", resp.token_usage.prompt_tokens)?;
        dict.set_item("completion_tokens", resp.token_usage.completion_tokens)?;
        dict.set_item("total_tokens", resp.token_usage.total_tokens)?;
        Ok(dict)
    }

    /// 同步 chat_with_tools:带工具定义(OpenAI Function Calling)
    ///
    /// `messages`: 同 `chat()`,每条是 `LLMMessage` 或含 role/content 的 dict。
    /// `tools`: list[dict],每个 dict 必含 `name`/`parameters`(JSON Schema),
    ///         可选 `description`。
    ///
    /// 返回 dict:
    ///   - `content`: 文本内容(仅工具调用时可能为空)
    ///   - `tool_calls`: JSON 字符串(`[{"id","function_name","arguments"}]`),无则为 "null"
    ///   - `finish_reason`: Stop / ToolCalls / Length / ContentFilter
    ///   - `prompt_tokens` / `completion_tokens` / `total_tokens`
    fn chat_with_tools<'py>(
        &self,
        py: Python<'py>,
        messages: Vec<Bound<'py, PyAny>>,
        tools: Vec<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let mut msgs: Vec<Message> = Vec::with_capacity(messages.len());
        for m in &messages {
            msgs.push(parse_py_message(m)?);
        }

        let mut tool_defs: Vec<ToolDefinition> = Vec::with_capacity(tools.len());
        for t in &tools {
            tool_defs.push(parse_py_tool_definition(py, t)?);
        }

        let backend = self.inner.clone();
        let resp = self
            .runtime
            .block_on(async move {
                backend
                    .lock()
                    .await
                    .complete_with_tools(&msgs, &tool_defs)
                    .await
            })
            .map_err(map_err)?;

        // tool_calls 序列化为 JSON 字符串(避免暴露 Rust ToolCall 结构给 Python)
        let tool_calls_json = match &resp.tool_calls {
            Some(calls) => serde_json::to_string(calls)
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?,
            None => "null".to_string(),
        };

        let dict = PyDict::new(py);
        dict.set_item("content", resp.content.unwrap_or_default())?;
        dict.set_item(
            "reasoning_content",
            resp.reasoning_content.unwrap_or_default(),
        )?;
        dict.set_item("tool_calls", tool_calls_json)?;
        dict.set_item("finish_reason", format!("{:?}", resp.finish_reason))?;
        dict.set_item("prompt_tokens", resp.token_usage.prompt_tokens)?;
        dict.set_item("completion_tokens", resp.token_usage.completion_tokens)?;
        dict.set_item("total_tokens", resp.token_usage.total_tokens)?;
        Ok(dict)
    }

    /// 同步 stream_chat:流式 chat completion,收集全部 `TokenDelta` 并转成 Python dict 列表
    ///
    /// `messages`: 同 `chat()`。
    ///
    /// 注意:为简化 PyO3 桥接,本方法**一次性收集整个流**再返回(非低延迟逐 chunk 推送);
    /// 真正的"边生成边推"留待后续用 channel/generator 桥实现。
    ///
    /// 返回 `list[dict]`,每个 dict 含 `type` 字段表示增量类型:
    ///   - `{"type": "content", "content": str}`
    ///   - `{"type": "reasoning", "content": str}`
    ///   - `{"type": "tool_call_start", "id": str, "name": str}`
    ///   - `{"type": "tool_call_delta", "id": str, "arguments": str}`
    ///   - `{"type": "done", "finish_reason": str}`
    fn stream_chat<'py>(
        &self,
        py: Python<'py>,
        messages: Vec<Bound<'py, PyAny>>,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let mut msgs: Vec<Message> = Vec::with_capacity(messages.len());
        for m in &messages {
            msgs.push(parse_py_message(m)?);
        }

        let backend = self.inner.clone();
        // block_on 一次性消费整个 SSE 流;item 为 Result<TokenDelta, LLMError>
        let deltas: Result<Vec<TokenDelta>, LLMError> = self.runtime.block_on(async move {
            // stream_complete 返回的 async_stream 类型不实现 Unpin,需 pin! 后才能 next()
            let mut stream = std::pin::pin!(backend.lock().await.stream_complete(&msgs));
            let mut out: Vec<TokenDelta> = Vec::new();
            while let Some(item) = stream.next().await {
                out.push(item?);
            }
            Ok(out)
        });

        let deltas = deltas.map_err(map_err)?;
        let mut result = Vec::with_capacity(deltas.len());
        for d in deltas {
            result.push(delta_to_pydict(py, d)?);
        }
        Ok(result)
    }

    /// 字符串表示
    fn __repr__(&self) -> String {
        "LLMBackend(OpenAICompatBackend)".to_string()
    }
}

/// 把 Rust `LLMError` 转为 Python 异常
fn map_err(e: LLMError) -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(e.to_string())
}

/// 解析单条 Python 消息,接受 `LLMMessage` 或 dict
fn parse_py_message(obj: &Bound<'_, PyAny>) -> PyResult<Message> {
    // 优先尝试 `LLMMessage` 实例(精确类型路径)
    if let Ok(pym) = obj.extract::<PyMessage>() {
        return Ok(pym.into());
    }
    // 回退到 dict
    if let Ok(d) = obj.cast::<PyDict>() {
        let role: String = d
            .get_item("role")?
            .and_then(|v| v.extract().ok())
            .ok_or_else(|| {
                pyo3::exceptions::PyValueError::new_err("dict message missing 'role'")
            })?;
        let content: String = d
            .get_item("content")?
            .and_then(|v| v.extract().ok())
            .unwrap_or_default();
        let tool_call_id: Option<String> = d
            .get_item("tool_call_id")?
            .and_then(|v| v.extract().ok())
            .flatten();
        let tool_calls: Option<String> = d
            .get_item("tool_calls")?
            .and_then(|v| v.extract().ok())
            .flatten();
        return Ok(Message::from(PyMessage {
            role,
            content,
            tool_call_id,
            tool_calls,
        }));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(
        "each message must be LLMMessage or dict",
    ))
}

/// 解析单个 Python 工具定义为 `ToolDefinition`
///
/// Python 端传入 dict:`{"name": str, "description"?: str, "parameters": {...}}`。
/// `parameters` 是 JSON Schema(dict),用 `pythonize` 转成 `serde_json::Value`。
fn parse_py_tool_definition(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<ToolDefinition> {
    let d = obj.cast::<PyDict>().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err(
            "tool must be a dict with name/description/parameters",
        )
    })?;

    let name: String = d
        .get_item("name")?
        .and_then(|v| v.extract::<String>().ok())
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("tool dict missing 'name'"))?;

    let description: String = d
        .get_item("description")?
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_default();

    let parameters: serde_json::Value = match d.get_item("parameters")? {
        Some(p) => pythonize(py, &p)?,
        None => serde_json::Value::Object(serde_json::Map::new()),
    };

    Ok(ToolDefinition {
        name,
        description,
        parameters,
    })
}

/// 把单个 `TokenDelta` 转成 Python dict(供 `stream_chat` 使用)
fn delta_to_pydict(py: Python<'_>, d: TokenDelta) -> PyResult<Bound<'_, PyDict>> {
    let dict = PyDict::new(py);
    match d {
        TokenDelta::Content(s) => {
            dict.set_item("type", "content")?;
            dict.set_item("content", s)?;
        }
        TokenDelta::Reasoning(s) => {
            dict.set_item("type", "reasoning")?;
            dict.set_item("content", s)?;
        }
        TokenDelta::ToolCallStart { id, name } => {
            dict.set_item("type", "tool_call_start")?;
            dict.set_item("id", id)?;
            dict.set_item("name", name)?;
        }
        TokenDelta::ToolCallDelta {
            id,
            arguments_delta,
        } => {
            dict.set_item("type", "tool_call_delta")?;
            dict.set_item("id", id)?;
            dict.set_item("arguments", arguments_delta)?;
        }
        TokenDelta::Done { finish_reason } => {
            dict.set_item("type", "done")?;
            dict.set_item("finish_reason", finish_reason)?;
        }
    }
    Ok(dict)
}

/// Python 端消息 DTO
///
/// 简化设计:role / content / tool_call_id / tool_calls(JSON 字符串)
/// 暴露为 `pyclass` 以便 Python 端可以直接构造。
///
/// `#[pyclass(from_py_object)]`:pyo3 0.28 起需要显式 opt-in FromPyObject
/// 才能在 `chat(messages: Vec<LLMMessage>)` 等函数签名中提取 pyclass 值。
#[pyclass(name = "LLMMessage", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyMessage {
    /// role:"system" | "user" | "assistant" | "tool"
    pub role: String,
    /// content 文本
    pub content: String,
    /// tool result 关联的 tool_call_id(可选)
    pub tool_call_id: Option<String>,
    /// tool_calls JSON 字符串(可选)
    pub tool_calls: Option<String>,
}

#[pymethods]
impl PyMessage {
    /// 构造 LLMMessage
    ///
    /// `tool_calls` 是 JSON 字符串(避免暴露 Rust 类型给 Python)。
    #[new]
    #[pyo3(signature = (role, content, tool_call_id=None, tool_calls=None))]
    fn new(
        role: String,
        content: String,
        tool_call_id: Option<String>,
        tool_calls: Option<String>,
    ) -> Self {
        Self {
            role,
            content,
            tool_call_id,
            tool_calls,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "LLMMessage(role={}, content={:?}{})",
            self.role,
            self.content,
            self.tool_call_id
                .as_ref()
                .map(|id| format!(", tool_call_id={id}"))
                .unwrap_or_default()
        )
    }
}

impl From<PyMessage> for Message {
    fn from(p: PyMessage) -> Self {
        let role = match p.role.as_str() {
            "system" => crate::types::Role::System,
            "assistant" => crate::types::Role::Assistant,
            "tool" => crate::types::Role::Tool,
            _ => crate::types::Role::User,
        };
        // tool_calls JSON 反序列化(失败则忽略)
        let tcs = p.tool_calls.and_then(|s| serde_json::from_str(&s).ok());
        Message {
            role,
            content: p.content,
            tool_call_id: p.tool_call_id,
            tool_calls: tcs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Role;
    use pretty_assertions::assert_eq;

    /// `PyMessage → Message` 角色映射覆盖测试:
    /// 验证 4 个 role 都正确,未知 role 降级为 User。
    #[test]
    fn py_message_role_mapping_covers_all_known_roles() {
        for (input_role, expected) in [
            ("system", Role::System),
            ("user", Role::User),
            ("assistant", Role::Assistant),
            ("tool", Role::Tool),
        ] {
            let m: Message =
                PyMessage::new(input_role.to_string(), "hi".to_string(), None, None).into();
            assert_eq!(m.role, expected, "input role: {input_role}");
        }

        // 未知 role 降级为 User(防御性默认值,避免下游 panic)
        let m: Message = PyMessage::new("alien".to_string(), "hi".to_string(), None, None).into();
        assert_eq!(m.role, Role::User);
    }

    /// `tool_calls` JSON 字段正确反序列化为 `Vec<ToolCall>`,
    /// 非法 JSON 必须降级为 `None`(不抛错),保持向后兼容。
    #[test]
    fn py_message_tool_calls_json_roundtrip() {
        // 合法 JSON(ToolCall 字段:id / function_name / arguments)
        let json = r#"[{"id":"t1","function_name":"foo","arguments":"{}"}]"#;
        let m: Message = PyMessage::new(
            "assistant".to_string(),
            "call".to_string(),
            None,
            Some(json.to_string()),
        )
        .into();
        let tcs = m.tool_calls.expect("tool_calls should be Some");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "t1");
        assert_eq!(tcs[0].function_name, "foo");
        assert_eq!(tcs[0].arguments, "{}");

        // 非法 JSON → None
        let m: Message = PyMessage::new(
            "assistant".to_string(),
            "x".to_string(),
            None,
            Some("not json".to_string()),
        )
        .into();
        assert!(m.tool_calls.is_none());
    }

    /// `tool_call_id` 直接透传
    #[test]
    fn py_message_tool_call_id_pass_through() {
        let m: Message = PyMessage::new(
            "tool".to_string(),
            "result".to_string(),
            Some("abc".to_string()),
            None,
        )
        .into();
        assert_eq!(m.tool_call_id.as_deref(), Some("abc"));
        assert_eq!(m.content, "result");
    }

    /// `__repr__` 应至少包含 role + content,方便调试
    #[test]
    fn py_message_repr_contains_role_and_content() {
        let p = PyMessage::new("user".to_string(), "hello".to_string(), None, None);
        let r = p.__repr__();
        assert!(r.contains("user"), "repr: {r}");
        assert!(r.contains("hello"), "repr: {r}");
    }
}
