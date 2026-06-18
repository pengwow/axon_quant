//! LLM HTTP 录制/回放中间件(vcr 风格)
//!
//! 用于 e2e 测试,避免每次都打真实 API:
//! - [`Mode::Replay`]:从 fixture 文件读响应(miss → panic)
//! - [`Mode::Record`]:转发到真实 backend,落盘 fixture
//! - [`Mode::Passthrough`][]:只转发,不存盘(临时调试)
//!
//! Fixture 文件名:`<fixtures_dir>/<test_name>/<model>/<key>.json`,
//! 其中 `key = sha256(url + method + canonical(body))` 前 12 hex。
//!
//! 录制前自动脱敏:
//! - request:`Authorization` / `x-api-key` / `host`
//! - response:`set-cookie` / `authorization`

use crate::backend::{LLMBackend, LLMError};
use crate::types::{LLMResponse, Message};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 录制/回放模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 内存回放(优先) + 文件回放(miss → panic)
    Replay,
    /// 转发到真实 backend + 落盘
    Record,
    /// 转发到真实 backend,不存盘
    Passthrough,
}

impl Mode {
    /// 从环境变量解析(`E2E_MODE`)
    pub fn from_env() -> Self {
        match std::env::var("E2E_MODE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "record" => Self::Record,
            "live" | "passthrough" => Self::Passthrough,
            _ => Self::Replay,
        }
    }
}

/// 录制请求
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordedRequest {
    /// 完整 URL
    pub url: String,
    /// HTTP method
    pub method: String,
    /// header 字典
    pub headers: BTreeMap<String, String>,
    /// body(JSON 值)
    pub body: serde_json::Value,
}

/// 录制响应
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordedResponse {
    /// HTTP 状态码
    pub status: u16,
    /// header 字典
    pub headers: BTreeMap<String, String>,
    /// body(JSON 值)
    pub body: serde_json::Value,
}

/// 顶层 fixture 结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    /// 格式版本
    pub version: u32,
    /// 录制 UTC 时间
    pub recorded_at: String,
    /// 模型名
    pub model: String,
    /// 请求
    pub request: RecordedRequest,
    /// 响应
    pub response: RecordedResponse,
}

/// 录制/回放中间件
pub struct RecordingLayer {
    /// 当前模式
    mode: Mode,
    /// fixture 根目录
    fixtures_dir: PathBuf,
    /// 当前测试名(子目录名)
    test_name: String,
    /// 内存回放缓存(同一进程内避免重复 IO)
    cache: Mutex<std::collections::HashMap<String, RecordedResponse>>,
}

impl RecordingLayer {
    /// 显式构造
    pub fn new(mode: Mode, fixtures_dir: PathBuf, test_name: impl Into<String>) -> Self {
        Self {
            mode,
            fixtures_dir,
            test_name: test_name.into(),
            cache: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// 从环境变量构造(常用)
    pub fn from_env(test_name: impl Into<String>) -> Self {
        let default = PathBuf::from("tests/e2e/common/fixtures");
        Self::new(Mode::from_env(), default, test_name)
    }

    /// 替换 fixtures 目录
    pub fn with_fixtures_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.fixtures_dir = dir.into();
        self
    }

    /// 替换模式
    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// 当前模式
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// fixture 子目录
    pub fn dir(&self) -> PathBuf {
        self.fixtures_dir.join(&self.test_name)
    }

    /// 计算 fixture key(SHA-256 url + method + canonical(body) 前 12 hex)
    pub fn fixture_key(req: &RecordedRequest) -> String {
        let canonical = canonicalize_body(&req.body);
        let mut hasher = Sha256::new();
        hasher.update(req.url.as_bytes());
        hasher.update(b"|");
        hasher.update(req.method.as_bytes());
        hasher.update(b"|");
        hasher.update(canonical.as_bytes());
        let digest = hasher.finalize();
        hex::encode(&digest[..6])
    }

    /// 构造完整 fixture 路径
    pub fn fixture_path(&self, req: &RecordedRequest) -> PathBuf {
        let model = req
            .body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        self.dir()
            .join(model)
            .join(format!("{}.json", Self::fixture_key(req)))
    }

    /// 读 fixture(回放时)
    fn read_fixture(&self, path: &Path) -> Result<RecordedResponse, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
        let fixture: Fixture =
            serde_json::from_str(&raw).map_err(|e| format!("parse {path:?}: {e}"))?;
        Ok(fixture.response)
    }

    /// 写 fixture(录制时)
    fn write_fixture(&self, path: &Path, req: &RecordedRequest, resp: &RecordedResponse) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let model = req
            .body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let fixture = Fixture {
            version: 1,
            recorded_at: now_iso8601(),
            model,
            request: req.clone(),
            response: resp.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&fixture) {
            let _ = std::fs::write(path, json);
        }
    }

    /// 转发请求到 backend(实际 HTTP 调用)
    async fn call_backend(
        &self,
        backend: &dyn LLMBackend,
        req: &RecordedRequest,
    ) -> Result<RecordedResponse, LLMError> {
        // 1. 反序列化 messages
        let messages: Vec<Message> = serde_json::from_value(
            req.body
                .get("messages")
                .cloned()
                .unwrap_or(serde_json::Value::Array(vec![])),
        )
        .map_err(|e| LLMError::Parse(format!("decode messages: {e}")))?;

        // 2. 调 backend
        let llm_resp: LLMResponse = backend.complete(&messages).await?;

        // 3. 包成 RecordedResponse
        let body_json = serde_json::to_value(ChatCompletionLikeResp::from(&llm_resp))
            .map_err(|e| LLMError::Parse(format!("encode resp: {e}")))?;
        Ok(RecordedResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: body_json,
        })
    }

    /// 主入口:走录制或回放,返回模拟的 HTTP 响应
    pub async fn send(
        &self,
        req: RecordedRequest,
        backend: &dyn LLMBackend,
    ) -> Result<RecordedResponse, LLMError> {
        let path = self.fixture_path(&req);
        let key = Self::fixture_key(&req);

        // 1. 先查内存缓存
        {
            let cache = self.cache.lock().expect("cache poisoned");
            if let Some(cached) = cache.get(&key) {
                return Ok(cached.clone());
            }
        }

        match self.mode {
            Mode::Replay => {
                if !path.exists() {
                    return Err(LLMError::Parse(format!(
                        "fixture missing: {path:?} (test={}, key={key})",
                        self.test_name
                    )));
                }
                let resp = self.read_fixture(&path).map_err(LLMError::Parse)?;
                let mut cache = self.cache.lock().expect("cache poisoned");
                cache.insert(key, resp.clone());
                Ok(resp)
            }
            Mode::Record => {
                let resp = self.call_backend(backend, &req).await?;
                self.write_fixture(&path, &req, &resp);
                let mut cache = self.cache.lock().expect("cache poisoned");
                cache.insert(key, resp.clone());
                Ok(resp)
            }
            Mode::Passthrough => {
                let resp = self.call_backend(backend, &req).await?;
                let mut cache = self.cache.lock().expect("cache poisoned");
                cache.insert(key, resp.clone());
                Ok(resp)
            }
        }
    }
}

// ─── 辅助 ─────────────────────────────────────────────

/// 脱敏 request 头(用于 fixture 落盘)
pub fn sanitize_request(mut req: RecordedRequest) -> RecordedRequest {
    for k in [
        "Authorization",
        "authorization",
        "x-api-key",
        "host",
        "Host",
    ] {
        req.headers.remove(k);
    }
    req
}

/// 脱敏 response 头
pub fn sanitize_response(mut resp: RecordedResponse) -> RecordedResponse {
    for k in ["set-cookie", "Set-Cookie", "authorization", "Authorization"] {
        resp.headers.remove(k);
    }
    resp
}

/// 对 body 做字典序排序后 serialize(JSON 字段顺序不影响 fixture key)
fn canonicalize_body(v: &serde_json::Value) -> String {
    let mut sorted = v.clone();
    sort_value(&mut sorted);
    serde_json::to_string(&sorted).unwrap_or_default()
}

fn sort_value(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            // 转 BTreeMap 再转回(serde_json::Map 默认 BTreeMap 行为)
            let entries: std::collections::BTreeMap<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| {
                    let mut v = v.clone();
                    sort_value(&mut v);
                    (k.clone(), v)
                })
                .collect();
            let mut new_map = serde_json::Map::new();
            for (k, v) in entries {
                new_map.insert(k, v);
            }
            *map = new_map;
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                sort_value(item);
            }
        }
        _ => {}
    }
}

fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 简化:用 chrono 转换会更精确,但避免依赖;这里返回秒级戳
    format!("epoch:{secs}")
}

// OpenAI 风格响应包装(用于把 LLMResponse 序列化成可比较的 JSON)
#[derive(Debug, Serialize, Deserialize)]
struct ChatCompletionLikeResp {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<ChoiceLike>,
    usage: UsageLike,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChoiceLike {
    index: u32,
    message: MessageLike,
    finish_reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct MessageLike {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<crate::types::ToolCall>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct UsageLike {
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
}

impl From<&LLMResponse> for ChatCompletionLikeResp {
    fn from(r: &LLMResponse) -> Self {
        let choice = ChoiceLike {
            index: 0,
            message: MessageLike {
                role: "assistant".to_string(),
                content: r.content.clone(),
                tool_calls: r.tool_calls.clone(),
            },
            finish_reason: match r.finish_reason {
                crate::types::FinishReason::Stop => "stop".to_string(),
                crate::types::FinishReason::Length => "length".to_string(),
                crate::types::FinishReason::ToolCalls => "tool_calls".to_string(),
                crate::types::FinishReason::ContentFilter => "content_filter".to_string(),
            },
        };
        Self {
            id: "chatcmpl-fixture".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "fixture".to_string(),
            choices: vec![choice],
            usage: UsageLike {
                prompt_tokens: r.token_usage.prompt_tokens,
                completion_tokens: r.token_usage.completion_tokens,
                total_tokens: r.token_usage.total_tokens,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_key_is_deterministic() {
        let req = RecordedRequest {
            url: "https://api.example.com/v1/chat".into(),
            method: "POST".into(),
            headers: BTreeMap::new(),
            body: serde_json::json!({"model": "x", "messages": []}),
        };
        let k1 = RecordingLayer::fixture_key(&req);
        let k2 = RecordingLayer::fixture_key(&req);
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 12);
    }

    #[test]
    fn fixture_key_independent_of_field_order() {
        let r1 = RecordedRequest {
            url: "https://api.example.com/v1/chat".into(),
            method: "POST".into(),
            headers: BTreeMap::new(),
            body: serde_json::json!({"model": "x", "messages": [], "temperature": 0.5}),
        };
        let r2 = RecordedRequest {
            url: "https://api.example.com/v1/chat".into(),
            method: "POST".into(),
            headers: BTreeMap::new(),
            body: serde_json::json!({"temperature": 0.5, "messages": [], "model": "x"}),
        };
        assert_eq!(
            RecordingLayer::fixture_key(&r1),
            RecordingLayer::fixture_key(&r2)
        );
    }

    #[test]
    fn sanitize_request_removes_auth() {
        let req = RecordedRequest {
            url: "x".into(),
            method: "POST".into(),
            headers: BTreeMap::from([
                ("Authorization".to_string(), "Bearer sk-xxx".to_string()),
                ("x-api-key".to_string(), "k".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ]),
            body: serde_json::json!({}),
        };
        let s = sanitize_request(req);
        assert!(!s.headers.contains_key("Authorization"));
        assert!(!s.headers.contains_key("x-api-key"));
        assert!(s.headers.contains_key("Content-Type"));
    }
}
