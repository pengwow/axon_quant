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

/// 兼容嵌套 runtime 上下文的同步块驱动（供 Python 绑定层同步 decide 使用）。
///
/// 三种调用场景：
/// - **内核回测路径**：当前线程已在某个 runtime 的 `block_on` 上下文里
///   （`Handle::try_current()` Ok）。此线程内再 `runtime.block_on` → tokio panic
///   "Cannot start a runtime from within a runtime"。
/// - 但嵌套时也不能用 `futures::executor::block_on` 直接驱动 future：
///   decide future 内含 reqwest IO / tokio timeout，依赖 runtime 的 reactor/timer，
///   futures executor 不驱动它们 → future 永久 pending，回测线程 hang。
/// - **纯 CLI / 独立线程**：无 runtime 上下文 → 共享 runtime 直接 `block_on`。
// 生产调用方在 python 绑定层（python/decision.rs）；非 python 构建下仅单测使用
#[cfg_attr(not(feature = "python"), allow(dead_code))]
pub(crate) fn block_on_compat<F>(rt: &tokio::runtime::Runtime, future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(_) => {
            // 嵌套：把 future spawn 到共享 multi_thread runtime（其 worker 正常
            // 驱动 reactor/timer），当前线程只等 JoinHandle —— 纯唤醒等待，
            // 不依赖任何 runtime 的 block_on，panic 与 hang 都消除，
            // 且与外层 runtime 是 current_thread 还是 multi_thread 无关。
            futures::executor::block_on(rt.spawn(future))
                .expect("decide future panicked in shared runtime")
        }
        Err(_) => rt.block_on(future),
    }
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

    #[test]
    fn nested_runtime_drives_tokio_timer() {
        // 复现回测 hang：R_K 为 current_thread runtime（reactor/timer 只随其
        // block_on 线程推进）。嵌套分支若用 futures::executor::block_on 直接
        // 驱动 tokio sleep，timer 无人驱动 → 永久 pending。watchdog 超时即失败。
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let shared = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let rk = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let out = rk.block_on(async {
                block_on_compat(&shared, async {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    42
                })
            });
            let _ = tx.send(out);
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(42),
            "嵌套 runtime 下 tokio timer future 必须能完成（futures::executor 直接驱动会 hang）"
        );
    }

    #[test]
    fn standalone_thread_uses_shared_runtime() {
        let shared = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(block_on_compat(&shared, async { 7 }), 7);
    }
}
