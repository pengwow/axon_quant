//! Swarm 错误类型

use thiserror::Error;

/// Swarm 错误
#[derive(Debug, Error)]
pub enum SwarmError {
    /// Agent 未找到
    #[error("agent not found: {0}")]
    AgentNotFound(String),

    /// 达到最大 Agent 数量
    #[error("max agents reached for role: {0:?}")]
    MaxAgentsReached(crate::swarm::agent::AgentRole),

    /// 投票超时
    #[error("vote timeout: {0}")]
    VoteTimeout(String),

    /// 消息发送失败
    #[error("message send failed: {0}")]
    MessageSendFailed(String),

    /// Agent 创建失败
    #[error("agent creation failed: {0}")]
    AgentCreationFailed(String),

    /// LLM 错误
    #[error("LLM error: {0}")]
    LLMError(#[from] crate::backend::LLMError),
}
