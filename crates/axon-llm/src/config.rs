//! 统一 LLM 配置类型
//!
//! 由 demo / python 端共用；支持 TOML 文件加载 + 字典构造 + 字段覆盖。
//!
//! Spec: docs/superpowers/specs/2026-06-15-axon-llm-ai-advanced-launch-design.md

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

/// 单个 backend 配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendConfig {
    /// backend 标识(ensemble 中用于区分)
    #[serde(default = "default_backend_name")]
    pub name: String,
    /// API base URL(末尾不带 `/`)
    pub base_url: String,
    /// API key
    pub api_key: String,
    /// 模型名
    pub model: String,
    /// 单次请求最大输出 token
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// 采样温度
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// HTTP 超时(秒)
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_backend_name() -> String {
    "primary".to_string()
}
fn default_max_tokens() -> u32 {
    1024
}
fn default_temperature() -> f32 {
    0.7
}
fn default_timeout_secs() -> u64 {
    60
}

/// 重试配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryConfig {
    /// 最大重试次数
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// 首次退避毫秒
    #[serde(default = "default_initial_backoff_ms")]
    pub initial_backoff_ms: u64,
    /// 最大退避毫秒
    #[serde(default = "default_max_backoff_ms")]
    pub max_backoff_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            initial_backoff_ms: default_initial_backoff_ms(),
            max_backoff_ms: default_max_backoff_ms(),
        }
    }
}

fn default_max_retries() -> u32 {
    3
}
fn default_initial_backoff_ms() -> u64 {
    200
}
fn default_max_backoff_ms() -> u64 {
    5000
}

/// explain 集成配置
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExplainConfig {
    /// 是否记录每次 LLM 决策到本地 store
    #[serde(default)]
    pub record_decisions: bool,
    /// 决策持久化路径(JSONL)
    #[serde(default)]
    pub store_path: Option<String>,
}

/// 顶层 LLM 配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LLMConfig {
    /// backend 列表(单 demo 时通常 length=1, ensemble 时 length=2..3)
    #[serde(default)]
    pub backends: Vec<BackendConfig>,
    /// 兼容字段:若使用单 backend 形式 `[backend]`,自动转为 `backends[0]`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendConfig>,
    /// 重试配置
    #[serde(default)]
    pub retry: RetryConfig,
    /// explain 配置
    #[serde(default)]
    pub explain: ExplainConfig,
}

/// 配置错误
#[derive(Debug, Error)]
pub enum ConfigError {
    /// 配置文件不存在
    #[error("config file not found: {path}")]
    NotFound {
        /// 缺失的文件路径
        path: String,
    },

    /// TOML 解析错误
    #[error("config parse error: {0}")]
    Parse(String),

    /// 字段验证失败
    #[error("validation: {field} {reason}")]
    Validation {
        /// 验证失败的字段
        field: String,
        /// 失败原因
        reason: String,
    },

    /// override 冲突
    #[error("override conflict: {0}")]
    OverrideConflict(String),
}

impl LLMConfig {
    /// 从 TOML 字符串解析
    #[allow(clippy::should_implement_trait)] // 命名与 toml::from_str 对齐
    pub fn from_str(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))
    }

    /// 从 TOML 文件加载
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|_| ConfigError::NotFound {
            path: path.display().to_string(),
        })?;
        let mut cfg: Self = toml::from_str(&raw).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.normalize_single_backend();
        cfg.validate()?;
        Ok(cfg)
    }

    /// 从 HashMap 字典构造(供 PyO3 桥用)
    pub fn from_dict(mut map: HashMap<String, serde_json::Value>) -> Result<Self, ConfigError> {
        // 接受 Python 风格下划线/驼峰键,统一转 snake_case
        let normalized: HashMap<String, serde_json::Value> =
            map.drain().map(|(k, v)| (to_snake_case(&k), v)).collect();
        // serde_json::Value -> toml::Value
        let json_str =
            serde_json::to_string(&normalized).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let json: serde_json::Value =
            serde_json::from_str(&json_str).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let toml_val: toml::Value =
            toml::Value::try_from(json).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let mut cfg: Self = toml_val
            .try_into()
            .map_err(|e: toml::de::Error| ConfigError::Parse(e.to_string()))?;
        cfg.normalize_single_backend();
        cfg.validate()?;
        Ok(cfg)
    }

    /// 兼容 `[backend]` 单 backend 形式,自动归一为 `backends[0]`
    fn normalize_single_backend(&mut self) {
        if self.backends.is_empty()
            && let Some(single) = self.backend.take()
        {
            self.backends.push(single);
        }
    }

    /// 验证配置合法性
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.backends.is_empty() {
            return Err(ConfigError::Validation {
                field: "backends".into(),
                reason: "at least one backend is required".into(),
            });
        }
        for (i, b) in self.backends.iter().enumerate() {
            if b.api_key.trim().is_empty() || b.api_key == "<set-me>" {
                return Err(ConfigError::Validation {
                    field: format!("backends[{i}].api_key"),
                    reason: "api_key is empty or unset placeholder".into(),
                });
            }
            if !b.base_url.starts_with("http://") && !b.base_url.starts_with("https://") {
                return Err(ConfigError::Validation {
                    field: format!("backends[{i}].base_url"),
                    reason: format!("must start with http:// or https://, got {}", b.base_url),
                });
            }
            if b.model.trim().is_empty() {
                return Err(ConfigError::Validation {
                    field: format!("backends[{i}].model"),
                    reason: "model is empty".into(),
                });
            }
        }
        Ok(())
    }

    /// 字段级 override,返回新 LLMConfig
    pub fn merged_override(mut self, ovr: LLMConfigOverride) -> Self {
        for b in self.backends.iter_mut() {
            if let Some(ref v) = ovr.api_key {
                b.api_key = v.clone();
            }
            if let Some(ref v) = ovr.model {
                b.model = v.clone();
            }
            if let Some(ref v) = ovr.base_url {
                b.base_url = v.clone();
            }
            if let Some(v) = ovr.max_tokens {
                b.max_tokens = v;
            }
            if let Some(v) = ovr.temperature {
                b.temperature = v;
            }
            if let Some(v) = ovr.timeout_secs {
                b.timeout_secs = v;
            }
        }
        self
    }

    /// 5 级 fallback 解析:
    ///   1. explicit_path
    ///   2. cwd/config.local.toml
    ///   3. cwd/config.toml
    ///   4. cwd/crates/axon-llm/demo/bin/config.toml
    ///   5. LLMConfig::default()(validate 必失败,要求显式 set api_key)
    pub fn resolve_with_fallback(
        explicit_path: Option<&Path>,
        cwd: &Path,
    ) -> Result<Self, ConfigError> {
        let candidates = if let Some(p) = explicit_path {
            vec![p.to_path_buf()]
        } else {
            vec![
                cwd.join("config.local.toml"),
                cwd.join("config.toml"),
                cwd.join("crates/axon-llm/demo/bin/config.toml"),
            ]
        };
        for path in candidates {
            if path.exists() {
                return Self::from_file(&path);
            }
        }
        // 兜底:返回 default 并 validate,提示用户需显式 set api_key
        let default = LLMConfig::default();
        default.validate()?;
        Ok(default)
    }

    /// 序列化为模板 TOML(可选是否包含敏感字段)
    pub fn to_template_toml(&self, include_secrets: bool) -> String {
        let mut clone = self.clone();
        if !include_secrets {
            for b in clone.backends.iter_mut() {
                b.api_key = "<set-me>".to_string();
            }
        }
        toml::to_string_pretty(&clone).unwrap_or_default()
    }
}

/// 单字段 override
#[derive(Debug, Clone, Default)]
pub struct LLMConfigOverride {
    /// 覆盖 api_key
    pub api_key: Option<String>,
    /// 覆盖 model
    pub model: Option<String>,
    /// 覆盖 base_url
    pub base_url: Option<String>,
    /// 覆盖 max_tokens
    pub max_tokens: Option<u32>,
    /// 覆盖 temperature
    pub temperature: Option<f32>,
    /// 覆盖 timeout_secs
    pub timeout_secs: Option<u64>,
}

/// 简单驼峰转 snake_case(供 from_dict 接受 Python 键)
fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_str_multi_backend() {
        let toml_str = r#"
            [[backends]]
            name = "primary"
            base_url = "https://api.deepseek.com/v1"
            api_key = "sk-xxx"
            model = "deepseek-chat"

            [[backends]]
            name = "arbiter"
            base_url = "https://api.openai.com/v1"
            api_key = "sk-yyy"
            model = "gpt-4o-mini"
        "#;
        let cfg = LLMConfig::from_str(toml_str).expect("parse");
        assert_eq!(cfg.backends.len(), 2);
        assert_eq!(cfg.backends[0].name, "primary");
        assert_eq!(cfg.backends[1].model, "gpt-4o-mini");
        assert_eq!(cfg.retry.max_retries, 3);
    }

    #[test]
    fn test_from_file_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(
            &path,
            r#"
            [[backends]]
            base_url = "https://api.deepseek.com/v1"
            api_key = "sk-xxx"
            model = "deepseek-chat"
        "#,
        )
        .unwrap();
        let cfg = LLMConfig::from_file(&path).unwrap();
        assert_eq!(cfg.backends.len(), 1);
        assert_eq!(cfg.backends[0].model, "deepseek-chat");
    }

    #[test]
    fn test_from_file_missing_returns_descriptive_error() {
        let result = LLMConfig::from_file(Path::new("/nonexistent/path.toml"));
        match result {
            Err(ConfigError::NotFound { path }) => assert!(path.contains("nonexistent")),
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_from_file_invalid_toml_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is = = not valid toml = = =").unwrap();
        let result = LLMConfig::from_file(&path);
        assert!(matches!(result, Err(ConfigError::Parse(_))));
    }

    #[test]
    fn test_from_dict_accepts_python_style_keys() {
        let mut map = HashMap::new();
        let mut b = serde_json::Map::new();
        b.insert(
            "base_url".to_string(),
            serde_json::Value::String("https://x.com/v1".into()),
        );
        b.insert("api_key".to_string(), serde_json::Value::String("k".into()));
        b.insert("model".to_string(), serde_json::Value::String("m".into()));
        map.insert(
            "backends".to_string(),
            serde_json::Value::Array(vec![serde_json::Value::Object(b)]),
        );
        let cfg = LLMConfig::from_dict(map).unwrap();
        assert_eq!(cfg.backends[0].model, "m");
        assert_eq!(cfg.backends[0].api_key, "k");
    }

    #[test]
    fn test_validate_rejects_empty_api_key() {
        let cfg = LLMConfig {
            backends: vec![BackendConfig {
                name: "x".into(),
                base_url: "https://x.com/v1".into(),
                api_key: "".into(),
                model: "m".into(),
                max_tokens: 1024,
                temperature: 0.7,
                timeout_secs: 60,
            }],
            backend: None,
            retry: RetryConfig::default(),
            explain: ExplainConfig::default(),
        };
        let result = cfg.validate();
        assert!(
            matches!(result, Err(ConfigError::Validation { ref field, .. }) if field == "backends[0].api_key")
        );
    }

    #[test]
    fn test_validate_rejects_non_http_base_url() {
        let cfg = LLMConfig {
            backends: vec![BackendConfig {
                name: "x".into(),
                base_url: "ftp://x.com/v1".into(),
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
        let result = cfg.validate();
        assert!(
            matches!(result, Err(ConfigError::Validation { ref field, .. }) if field == "backends[0].base_url")
        );
    }

    #[test]
    fn test_validate_rejects_placeholder_api_key() {
        let cfg = LLMConfig {
            backends: vec![BackendConfig {
                name: "x".into(),
                base_url: "https://x.com/v1".into(),
                api_key: "<set-me>".into(),
                model: "m".into(),
                max_tokens: 1024,
                temperature: 0.7,
                timeout_secs: 60,
            }],
            backend: None,
            retry: RetryConfig::default(),
            explain: ExplainConfig::default(),
        };
        let result = cfg.validate();
        assert!(
            matches!(result, Err(ConfigError::Validation { ref field, .. }) if field == "backends[0].api_key")
        );
    }

    #[test]
    fn test_validate_rejects_empty_backends() {
        let cfg = LLMConfig {
            backends: vec![],
            backend: None,
            retry: RetryConfig::default(),
            explain: ExplainConfig::default(),
        };
        let result = cfg.validate();
        assert!(
            matches!(result, Err(ConfigError::Validation { ref field, .. }) if field == "backends")
        );
    }

    #[test]
    fn test_override_merges_partially() {
        let mut cfg = LLMConfig::default();
        cfg.backends.push(BackendConfig {
            name: "x".into(),
            base_url: "https://a.com/v1".into(),
            api_key: "k1".into(),
            model: "m1".into(),
            max_tokens: 1024,
            temperature: 0.7,
            timeout_secs: 60,
        });
        let ovr = LLMConfigOverride {
            api_key: Some("k2".into()),
            model: Some("m2".into()),
            ..Default::default()
        };
        let merged = cfg.merged_override(ovr);
        assert_eq!(merged.backends[0].api_key, "k2");
        assert_eq!(merged.backends[0].model, "m2");
        assert_eq!(merged.backends[0].base_url, "https://a.com/v1");
    }

    #[test]
    fn test_override_preserves_unset_fields() {
        let mut cfg = LLMConfig::default();
        cfg.backends.push(BackendConfig {
            name: "x".into(),
            base_url: "https://a.com/v1".into(),
            api_key: "k".into(),
            model: "m".into(),
            max_tokens: 2048,
            temperature: 0.5,
            timeout_secs: 90,
        });
        let ovr = LLMConfigOverride::default();
        let merged = cfg.merged_override(ovr);
        assert_eq!(merged.backends[0].max_tokens, 2048);
        assert_eq!(merged.backends[0].temperature, 0.5);
    }

    #[test]
    fn test_resolve_fallback_priority_explicit_over_local() {
        let dir = tempfile::tempdir().unwrap();
        let explicit = dir.path().join("explicit.toml");
        std::fs::write(
            &explicit,
            r#"
            [[backends]]
            base_url = "https://explicit.com/v1"
            api_key = "explicit-key"
            model = "explicit-model"
        "#,
        )
        .unwrap();
        // 同时放 config.local.toml;explicit 应优先
        std::fs::write(
            dir.path().join("config.local.toml"),
            r#"
            [[backends]]
            base_url = "https://local.com/v1"
            api_key = "local-key"
            model = "local-model"
        "#,
        )
        .unwrap();
        let resolved =
            LLMConfig::resolve_with_fallback(Some(explicit.as_path()), dir.path()).unwrap();
        assert_eq!(resolved.backends[0].base_url, "https://explicit.com/v1");
    }

    #[test]
    fn test_resolve_fallback_priority_local_over_repo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.local.toml"),
            r#"
            [[backends]]
            base_url = "https://local.com/v1"
            api_key = "local-key"
            model = "local-model"
        "#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
            [[backends]]
            base_url = "https://repo.com/v1"
            api_key = "repo-key"
            model = "repo-model"
        "#,
        )
        .unwrap();
        let resolved = LLMConfig::resolve_with_fallback(None, dir.path()).unwrap();
        assert_eq!(resolved.backends[0].base_url, "https://local.com/v1");
    }

    #[test]
    fn test_resolve_fallback_uses_builtin_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        // 不放任何文件
        let result = LLMConfig::resolve_with_fallback(None, dir.path());
        // 返回 default(),但 validate 必失败
        assert!(result.is_err());
    }

    #[test]
    fn test_save_template_redacts_secrets() {
        let mut cfg = LLMConfig::default();
        cfg.backends.push(BackendConfig {
            name: "x".into(),
            base_url: "https://a.com/v1".into(),
            api_key: "real-secret".into(),
            model: "m".into(),
            max_tokens: 1024,
            temperature: 0.7,
            timeout_secs: 60,
        });
        let toml_str = cfg.to_template_toml(false);
        assert!(toml_str.contains("<set-me>"));
        assert!(!toml_str.contains("real-secret"));

        let with_secrets = cfg.to_template_toml(true);
        assert!(with_secrets.contains("real-secret"));
    }
}
