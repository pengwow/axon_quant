//! SDM 决策后端抽象与统一错误类型

use async_trait::async_trait;

use super::types::{DecisionRequest, DecisionResponse};

/// SDM 决策调用的统一错误类型
#[derive(Debug, thiserror::Error)]
pub enum DecisionError {
    /// 网络错误（瞬时，可重试）
    #[error("network error: {0}")]
    Network(String),

    /// 认证失败（不可重试）
    #[error("auth error: {0}")]
    Auth(String),

    /// 限流（携带服务端 Retry-After 秒数，可空）
    #[error("rate limited (retry_after={retry_after:?}s)")]
    RateLimited {
        /// 服务端建议等待秒数，未知为 None
        retry_after: Option<u64>,
    },

    /// 响应解析失败（不可重试）
    #[error("parse error: {0}")]
    Parse(String),

    /// 后端通用错误（含 replay miss；5xx 可重试）
    #[error("backend error: {0}")]
    Backend(String),
}

impl DecisionError {
    /// 是否值得重试，与 `LLMError::is_retryable` 口径一致
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Network(_) | Self::RateLimited { .. } | Self::Backend(_)
        )
    }

    /// 错误判别名（用于 cascade 写 fallback_reason，不对外暴露内部结构）
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Network(_) => "Network",
            Self::Auth(_) => "Auth",
            Self::RateLimited { .. } => "RateLimited",
            Self::Parse(_) => "Parse",
            Self::Backend(_) => "Backend",
        }
    }

    /// 错误内部详情（供 cascade 拼接 fallback_reason；kind() 的稳定别名保持不变）
    pub fn detail(&self) -> String {
        match self {
            Self::Network(msg) | Self::Auth(msg) | Self::Parse(msg) | Self::Backend(msg) => {
                msg.clone()
            }
            Self::RateLimited { retry_after } => match retry_after {
                Some(secs) => format!("retry_after={secs}s"),
                None => "retry_after unknown".to_string(),
            },
        }
    }
}

/// SDM 决策后端 trait：所有实现（stub / replay / http）必须满足
#[async_trait]
pub trait DecisionBackend: Send + Sync {
    /// 提交一次结构化决策请求
    async fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecisionError>;

    /// 返回该后端的模型标识（stub 为 `sdm-stub`，http 为配置的 model）
    fn model_name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_matches_llm_error_policy() {
        assert!(DecisionError::Network("x".into()).is_retryable());
        assert!(DecisionError::RateLimited { retry_after: None }.is_retryable());
        assert!(DecisionError::Backend("5xx".into()).is_retryable());
        assert!(!DecisionError::Auth("bad key".into()).is_retryable());
        assert!(!DecisionError::Parse("bad json".into()).is_retryable());
    }

    #[test]
    fn kind_names_are_stable() {
        assert_eq!(DecisionError::Network("x".into()).kind(), "Network");
        assert_eq!(DecisionError::Parse("x".into()).kind(), "Parse");
        assert_eq!(
            DecisionError::RateLimited {
                retry_after: Some(2)
            }
            .kind(),
            "RateLimited"
        );
    }

    #[test]
    fn detail_returns_inner_message() {
        assert_eq!(DecisionError::Network("boom".into()).detail(), "boom");
        assert_eq!(DecisionError::Parse("bad json".into()).detail(), "bad json");
        assert_eq!(
            DecisionError::RateLimited {
                retry_after: Some(2)
            }
            .detail(),
            "retry_after=2s"
        );
        assert_eq!(
            DecisionError::RateLimited { retry_after: None }.detail(),
            "retry_after unknown"
        );
    }
}
