//! Agent 唯一标识和角色定义

use serde::{Deserialize, Serialize};

/// Agent 唯一标识
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    /// 创建新的 AgentId
    pub fn new(role: AgentRole) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(format!("{:?}_{}", role, id))
    }

    /// 从字符串创建
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// 获取 ID 字符串
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Agent 角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentRole {
    /// 市场分析
    Market,
    /// 风控
    Risk,
    /// 执行
    Execution,
    /// 审计
    Audit,
}

/// Agent 状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// 空闲
    Idle,
    /// 推理中
    Thinking,
    /// 投票中
    Voting,
    /// 执行中
    Executing,
    /// 故障
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_id_creation() {
        let id = AgentId::new(AgentRole::Market);
        assert!(!id.as_str().is_empty());
        assert!(id.as_str().contains("Market"));
    }

    #[test]
    fn test_agent_id_from_string() {
        let id = AgentId::from_string("custom_id");
        assert_eq!(id.as_str(), "custom_id");
    }

    #[test]
    fn test_agent_id_equality() {
        let id1 = AgentId::from_string("test");
        let id2 = AgentId::from_string("test");
        let id3 = AgentId::from_string("other");
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_agent_role_variants() {
        assert_ne!(AgentRole::Market, AgentRole::Risk);
        assert_ne!(AgentRole::Risk, AgentRole::Execution);
        assert_ne!(AgentRole::Execution, AgentRole::Audit);
    }

    #[test]
    fn test_agent_status_variants() {
        assert_ne!(AgentStatus::Idle, AgentStatus::Thinking);
        assert_ne!(AgentStatus::Thinking, AgentStatus::Voting);
        assert_ne!(AgentStatus::Voting, AgentStatus::Executing);
        assert_ne!(AgentStatus::Executing, AgentStatus::Failed);
    }
}
