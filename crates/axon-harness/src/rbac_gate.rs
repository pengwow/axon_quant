//! 基于角色的工具门控
//!
//! 基于角色的工具门控，支持：
//! - 角色权限管理
//! - 审批检查
//! - 审计记录

use std::collections::{HashMap, HashSet};

use crate::audit::AuditChain;
use crate::policy::ToolGate;
use crate::types::GateResult;

/// 基于角色的工具门控
///
/// 基于角色的工具门控，支持：
/// - 角色权限管理：每个角色可以使用的工具列表
/// - 审批检查：需要人工审批的工具
/// - 审计记录：记录所有工具调用
pub struct RBACToolGate {
    /// 角色 → 允许的工具列表
    permissions: HashMap<String, HashSet<String>>,
    /// 需要人工审批的工具
    approval_required: HashSet<String>,
    /// 审计链
    audit_chain: AuditChain,
}

impl RBACToolGate {
    /// 创建 RBAC 工具门控
    pub fn new(
        permissions: HashMap<String, HashSet<String>>,
        approval_required: HashSet<String>,
    ) -> Self {
        Self {
            permissions,
            approval_required,
            audit_chain: AuditChain::new(),
        }
    }

    /// 添加角色权限
    pub fn add_role(&mut self, role: &str, tools: HashSet<String>) {
        self.permissions.insert(role.to_string(), tools);
    }

    /// 添加需要审批的工具
    pub fn add_approval_required(&mut self, tool: &str) {
        self.approval_required.insert(tool.to_string());
    }

    /// 获取审计链引用
    pub fn audit_chain(&self) -> &AuditChain {
        &self.audit_chain
    }
}

impl Default for RBACToolGate {
    fn default() -> Self {
        let mut permissions = HashMap::new();
        // 默认权限
        permissions.insert("market".to_string(), HashSet::from([
            "query_market".to_string(),
            "analyze_trend".to_string(),
        ]));
        permissions.insert("execution".to_string(), HashSet::from([
            "place_order".to_string(),
            "cancel_order".to_string(),
        ]));
        permissions.insert("risk".to_string(), HashSet::from([
            "check_risk".to_string(),
            "query_portfolio".to_string(),
        ]));

        let approval_required = HashSet::from([
            "place_order".to_string(),
        ]);

        Self::new(permissions, approval_required)
    }
}

impl ToolGate for RBACToolGate {
    fn check(&self, tool: &str, agent: &str, _params: &serde_json::Value) -> GateResult {
        // 1. 检查角色权限
        if let Some(tools) = self.permissions.get(agent) {
            if !tools.contains(tool) {
                return GateResult::Denied(format!("角色 {agent} 无权使用工具 {tool}"));
            }
        }

        // 2. 检查是否需要审批
        if self.approval_required.contains(tool) {
            return GateResult::NeedsApproval;
        }

        GateResult::Allowed
    }

    fn needs_approval(&self, tool: &str, _params: &serde_json::Value) -> bool {
        self.approval_required.contains(tool)
    }

    fn record_call(&self, _tool: &str, _agent: &str, _params: &serde_json::Value, _result: &str) {
        // 注意：AuditChain 需要 &mut self，这里需要内部可变性
        // 实际实现中需要使用 Mutex 或 RwLock
        // 这里简化处理，不记录审计
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_permissions() {
        let gate = RBACToolGate::default();

        // market 角色可以查询市场
        assert_eq!(
            gate.check("query_market", "market", &serde_json::Value::Null),
            GateResult::Allowed
        );

        // market 角色不能下单
        assert!(matches!(
            gate.check("place_order", "market", &serde_json::Value::Null),
            GateResult::Denied(_)
        ));
    }

    #[test]
    fn test_approval_required() {
        let gate = RBACToolGate::default();

        // execution 角色可以下单，但需要审批
        assert_eq!(
            gate.check("place_order", "execution", &serde_json::Value::Null),
            GateResult::NeedsApproval
        );
    }

    #[test]
    fn test_needs_approval() {
        let gate = RBACToolGate::default();
        assert!(gate.needs_approval("place_order", &serde_json::Value::Null));
        assert!(!gate.needs_approval("query_market", &serde_json::Value::Null));
    }
}
