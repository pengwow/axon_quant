//! axon-llm Python 绑定契约测试
//!
//! 验证 Rust 端 Python 绑定的关键公开契约:
//! - `LlmConfig::from_dict` 接受 dict 形式的配置
//! - `OpenAICompatConfig::from_llm_config` 从 LlmConfig 构造
//! - `Message` 类型与 PyMessage 字段对齐(role / content / tool_call_id / tool_calls)
//!
//! 这些测试作为「contract tests」放在 integration tests 目录,
//! 避免在 lib 模式下导出 test-only 标记。
//!
//! ## Feature gate
//!
//! `OpenAICompatConfig` 在 `backends` feature 下才存在,
//! 所以整个测试用 `#[cfg(feature = "backends")]` 保护;
//! 启用 `python` feature 会隐含启用 `backends`。

#![cfg(feature = "backends")]

use std::collections::HashMap;

use axon_llm::backends::OpenAICompatConfig;
use axon_llm::config::LlmConfig;
use axon_llm::types::{Message, Role, ToolCall};
use pretty_assertions::assert_eq;

#[test]
fn llm_config_from_dict_supports_full_payload() {
    // 完整 dict 配置(模拟 Python 端 make_backend 的入参)
    let mut map = HashMap::new();
    let mut b = HashMap::new();
    b.insert(
        "base_url".to_string(),
        serde_json::json!("https://api.example.com/v1"),
    );
    b.insert("api_key".to_string(), serde_json::json!("sk-test"));
    b.insert("model".to_string(), serde_json::json!("mimo-v2.5"));
    b.insert("max_tokens".to_string(), serde_json::json!(512));
    b.insert("temperature".to_string(), serde_json::json!(0.5));
    b.insert("timeout_secs".to_string(), serde_json::json!(30));
    map.insert("backends".to_string(), serde_json::json!([b]));
    map.insert(
        "retry".to_string(),
        serde_json::json!({
            "max_retries": 5,
            "initial_backoff_ms": 100,
            "max_backoff_ms": 2000,
        }),
    );

    let cfg = LlmConfig::from_dict(map).expect("should parse");
    assert_eq!(cfg.backends.len(), 1);
    assert_eq!(cfg.backends[0].model, "mimo-v2.5");
    assert_eq!(cfg.retry.max_retries, 5);
}

#[test]
fn openai_compat_from_llm_config_uses_first_backend() {
    // 多 backend 列表时,取 index=0
    let mut map = HashMap::new();
    let b1: HashMap<String, serde_json::Value> = [
        ("base_url".to_string(), serde_json::json!("https://a/v1")),
        ("api_key".to_string(), serde_json::json!("sk-1")),
        ("model".to_string(), serde_json::json!("model-a")),
    ]
    .into_iter()
    .collect();
    let b2: HashMap<String, serde_json::Value> = [
        ("base_url".to_string(), serde_json::json!("https://b/v1")),
        ("api_key".to_string(), serde_json::json!("sk-2")),
        ("model".to_string(), serde_json::json!("model-b")),
    ]
    .into_iter()
    .collect();
    map.insert("backends".to_string(), serde_json::json!([b1, b2]));

    let cfg = LlmConfig::from_dict(map).expect("should parse");
    let compat = OpenAICompatConfig::from_llm_config(&cfg, 0).expect("should build");
    assert_eq!(compat.base_url, "https://a/v1");
    assert_eq!(compat.model, "model-a");
    assert_eq!(compat.api_key, "sk-1");

    // index=1 取第二个
    let compat2 = OpenAICompatConfig::from_llm_config(&cfg, 1).expect("should build idx=1");
    assert_eq!(compat2.model, "model-b");
}

#[test]
fn openai_compat_from_llm_config_out_of_range_returns_error() {
    let mut map = HashMap::new();
    let b: HashMap<String, serde_json::Value> = [
        ("base_url".to_string(), serde_json::json!("https://a/v1")),
        ("api_key".to_string(), serde_json::json!("sk-1")),
        ("model".to_string(), serde_json::json!("model-a")),
    ]
    .into_iter()
    .collect();
    map.insert("backends".to_string(), serde_json::json!([b]));
    let cfg = LlmConfig::from_dict(map).expect("should parse");
    let err = OpenAICompatConfig::from_llm_config(&cfg, 5).expect_err("index 5 should be OOB");
    assert!(err.to_string().to_lowercase().contains("not found"));
}

#[test]
fn message_construction_matches_python_payload() {
    // 模拟 PyMessage 转 Message 的路径
    let m = Message {
        role: Role::User,
        content: "hello".to_string(),
        tool_call_id: None,
        tool_calls: Some(vec![ToolCall {
            id: "t1".to_string(),
            function_name: "search".to_string(),
            arguments: "{}".to_string(),
        }]),
    };
    assert_eq!(m.role, Role::User);
    assert_eq!(m.content, "hello");
    assert_eq!(m.tool_calls.as_ref().unwrap().len(), 1);
    assert_eq!(m.tool_calls.as_ref().unwrap()[0].id, "t1");
}
