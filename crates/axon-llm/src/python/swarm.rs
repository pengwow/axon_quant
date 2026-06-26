//! Swarm 模块 Python 绑定
//!
//! 暴露 `SwarmOrchestrator` / `SwarmConfig` / `AgentRole` / `VoteProposal` / `VoteResult` 等。
//! 作为 `axon_quant.llm.swarm` 子模块注册。

use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::prelude::*;
use tokio::sync::mpsc;

use crate::swarm::agent::{AgentId, AgentRole as RustAgentRole, AgentStatus as RustAgentStatus};
use crate::swarm::message::{
    MarketSignal as RustMarketSignal, SignalType as RustSignalType,
    VoteProposal as RustVoteProposal, VoteResult as RustVoteResult, VoteType as RustVoteType,
};
use crate::swarm::orchestrator::{AgentHandle, SwarmConfig as RustSwarmConfig, SwarmOrchestrator};
use crate::swarm::vote::VoteResponse;

// ═══════════════════════════════════════════════════════════════════════════
// 枚举
// ═══════════════════════════════════════════════════════════════════════════

/// Agent 角色枚举
#[pyclass(name = "AgentRole", from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyAgentRole {
    /// 市场分析
    Market,
    /// 风控
    Risk,
    /// 执行
    Execution,
    /// 审计
    Audit,
}

impl From<PyAgentRole> for RustAgentRole {
    fn from(role: PyAgentRole) -> Self {
        match role {
            PyAgentRole::Market => RustAgentRole::Market,
            PyAgentRole::Risk => RustAgentRole::Risk,
            PyAgentRole::Execution => RustAgentRole::Execution,
            PyAgentRole::Audit => RustAgentRole::Audit,
        }
    }
}

impl From<RustAgentRole> for PyAgentRole {
    fn from(role: RustAgentRole) -> Self {
        match role {
            RustAgentRole::Market => PyAgentRole::Market,
            RustAgentRole::Risk => PyAgentRole::Risk,
            RustAgentRole::Execution => PyAgentRole::Execution,
            RustAgentRole::Audit => PyAgentRole::Audit,
        }
    }
}

/// Agent 状态枚举
#[pyclass(name = "AgentStatus", from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyAgentStatus {
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

impl From<RustAgentStatus> for PyAgentStatus {
    fn from(status: RustAgentStatus) -> Self {
        match status {
            RustAgentStatus::Idle => PyAgentStatus::Idle,
            RustAgentStatus::Thinking => PyAgentStatus::Thinking,
            RustAgentStatus::Voting => PyAgentStatus::Voting,
            RustAgentStatus::Executing => PyAgentStatus::Executing,
            RustAgentStatus::Failed => PyAgentStatus::Failed,
        }
    }
}

/// 投票类型枚举
#[pyclass(name = "VoteType", from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyVoteType {
    /// 交易决策
    TradeDecision,
    /// 紧急止损
    EmergencyStop,
    /// 策略调整
    StrategyAdjustment,
}

impl From<PyVoteType> for RustVoteType {
    fn from(t: PyVoteType) -> Self {
        match t {
            PyVoteType::TradeDecision => RustVoteType::TradeDecision,
            PyVoteType::EmergencyStop => RustVoteType::EmergencyStop,
            PyVoteType::StrategyAdjustment => RustVoteType::StrategyAdjustment,
        }
    }
}

/// 信号类型枚举
#[pyclass(name = "SignalType", from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PySignalType {
    /// 买入
    Buy,
    /// 卖出
    Sell,
    /// 持有
    Hold,
}

impl From<PySignalType> for RustSignalType {
    fn from(t: PySignalType) -> Self {
        match t {
            PySignalType::Buy => RustSignalType::Buy,
            PySignalType::Sell => RustSignalType::Sell,
            PySignalType::Hold => RustSignalType::Hold,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 数据结构
// ═══════════════════════════════════════════════════════════════════════════

/// Swarm 配置
#[pyclass(name = "SwarmConfig", from_py_object)]
#[derive(Debug, Clone)]
pub struct PySwarmConfig {
    inner: RustSwarmConfig,
}

#[pymethods]
impl PySwarmConfig {
    #[new]
    #[pyo3(signature = (vote_timeout_ms=5000))]
    fn new(vote_timeout_ms: u64) -> Self {
        Self {
            inner: RustSwarmConfig {
                vote_timeout_ms,
                ..Default::default()
            },
        }
    }

    /// 获取 vote_timeout_ms
    #[getter]
    fn vote_timeout_ms(&self) -> u64 {
        self.inner.vote_timeout_ms
    }

    fn __repr__(&self) -> String {
        format!(
            "SwarmConfig(vote_timeout_ms={})",
            self.inner.vote_timeout_ms
        )
    }
}

/// 投票提案
#[pyclass(name = "VoteProposal", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyVoteProposal {
    inner: RustVoteProposal,
}

#[pymethods]
impl PyVoteProposal {
    #[new]
    fn new(
        proposal_id: String,
        proposal_type: PyVoteType,
        content: String,
        deadline_ms: i64,
    ) -> Self {
        Self {
            inner: RustVoteProposal {
                proposal_id,
                proposal_type: proposal_type.into(),
                content,
                deadline_ms,
            },
        }
    }

    #[getter]
    fn proposal_id(&self) -> &str {
        &self.inner.proposal_id
    }

    #[getter]
    fn proposal_type(&self) -> PyVoteType {
        match self.inner.proposal_type {
            RustVoteType::TradeDecision => PyVoteType::TradeDecision,
            RustVoteType::EmergencyStop => PyVoteType::EmergencyStop,
            RustVoteType::StrategyAdjustment => PyVoteType::StrategyAdjustment,
        }
    }

    #[getter]
    fn content(&self) -> &str {
        &self.inner.content
    }

    #[getter]
    fn deadline_ms(&self) -> i64 {
        self.inner.deadline_ms
    }

    fn __repr__(&self) -> String {
        format!(
            "VoteProposal(id='{}', type={:?}, content='{}')",
            self.inner.proposal_id, self.inner.proposal_type, self.inner.content
        )
    }
}

/// 投票结果
#[pyclass(name = "VoteResult", skip_from_py_object)]
#[derive(Debug, Clone)]
pub struct PyVoteResult {
    inner: RustVoteResult,
}

#[pymethods]
impl PyVoteResult {
    #[getter]
    fn proposal_id(&self) -> &str {
        &self.inner.proposal_id
    }

    #[getter]
    fn passed(&self) -> bool {
        self.inner.passed
    }

    #[getter]
    fn approve_count(&self) -> usize {
        self.inner.approve_count
    }

    #[getter]
    fn reject_count(&self) -> usize {
        self.inner.reject_count
    }

    #[getter]
    fn abstain_count(&self) -> usize {
        self.inner.abstain_count
    }

    fn __repr__(&self) -> String {
        format!(
            "VoteResult(proposal='{}', passed={}, approve={}, reject={})",
            self.inner.proposal_id,
            self.inner.passed,
            self.inner.approve_count,
            self.inner.reject_count
        )
    }
}

/// 市场信号
#[pyclass(name = "MarketSignal", from_py_object)]
#[derive(Debug, Clone)]
pub struct PyMarketSignal {
    inner: RustMarketSignal,
}

#[pymethods]
impl PyMarketSignal {
    #[new]
    fn new(symbol: String, signal_type: PySignalType, confidence: f64, reasoning: String) -> Self {
        Self {
            inner: RustMarketSignal {
                symbol,
                signal_type: signal_type.into(),
                confidence,
                reasoning,
            },
        }
    }

    #[getter]
    fn symbol(&self) -> &str {
        &self.inner.symbol
    }

    #[getter]
    fn signal_type(&self) -> PySignalType {
        match self.inner.signal_type {
            RustSignalType::Buy => PySignalType::Buy,
            RustSignalType::Sell => PySignalType::Sell,
            RustSignalType::Hold => PySignalType::Hold,
        }
    }

    #[getter]
    fn confidence(&self) -> f64 {
        self.inner.confidence
    }

    #[getter]
    fn reasoning(&self) -> &str {
        &self.inner.reasoning
    }

    fn __repr__(&self) -> String {
        format!(
            "MarketSignal(symbol='{}', type={:?}, confidence={:.2})",
            self.inner.symbol, self.inner.signal_type, self.inner.confidence
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// SwarmOrchestrator
// ═══════════════════════════════════════════════════════════════════════════

/// Swarm 编排器 — Agent 生命周期管理、消息路由、投票共识
#[pyclass(name = "SwarmOrchestrator")]
pub struct PySwarmOrchestrator {
    inner: Arc<Mutex<SwarmOrchestrator>>,
}

#[pymethods]
impl PySwarmOrchestrator {
    #[new]
    fn new(config: &PySwarmConfig) -> Self {
        let (tx, rx) = mpsc::channel(1000);
        let orchestrator = SwarmOrchestrator::new(config.inner.clone(), rx, tx);
        Self {
            inner: Arc::new(Mutex::new(orchestrator)),
        }
    }

    /// 注册 Agent
    ///
    /// Args:
    ///     agent_id: Agent 唯一标识
    ///     role: Agent 角色 (AgentRole 枚举)
    ///
    /// Raises:
    ///     ValueError: 达到最大 Agent 数量
    fn register_agent(&self, agent_id: &str, role: PyAgentRole) -> PyResult<()> {
        let (agent_tx, _agent_rx) = mpsc::channel(100);
        let handle = AgentHandle {
            id: AgentId::from_string(agent_id),
            role: role.into(),
            status: RustAgentStatus::Idle,
            sender: agent_tx,
        };
        self.inner
            .lock()
            .register_agent(handle)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
    }

    /// 注销 Agent
    ///
    /// Args:
    ///     agent_id: Agent 唯一标识
    ///
    /// Returns:
    ///     True if removed, False if not found
    fn unregister_agent(&self, agent_id: &str) -> bool {
        self.inner
            .lock()
            .unregister_agent(&AgentId::from_string(agent_id))
            .is_some()
    }

    /// 获取 Agent 总数
    fn agent_count(&self) -> usize {
        self.inner.lock().agent_count()
    }

    /// 获取指定角色的 Agent 数量
    fn agent_count_by_role(&self, role: PyAgentRole) -> usize {
        self.inner.lock().agent_count_by_role(role.into())
    }

    /// 获取 Agent 状态
    ///
    /// Args:
    ///     agent_id: Agent 唯一标识
    ///
    /// Returns:
    ///     AgentStatus or None if not found
    fn agent_status(&self, agent_id: &str) -> Option<PyAgentStatus> {
        self.inner
            .lock()
            .agent_status(&AgentId::from_string(agent_id))
            .map(PyAgentStatus::from)
    }

    /// 发起投票
    ///
    /// Args:
    ///     proposal: VoteProposal 实例
    ///
    /// Returns:
    ///     提案 ID
    fn create_vote(&self, proposal: &PyVoteProposal) -> String {
        self.inner.lock().create_vote(proposal.inner.clone())
    }

    /// 提交投票响应
    ///
    /// Args:
    ///     proposal_id: 提案 ID
    ///     voter: 投票者 Agent ID
    ///     approved: 是否赞成
    ///     reasoning: 投票理由
    ///     confidence: 置信度 (0.0-1.0)
    fn submit_vote(
        &self,
        proposal_id: &str,
        voter: &str,
        approved: bool,
        reasoning: &str,
        confidence: f64,
    ) {
        let response = VoteResponse {
            proposal_id: proposal_id.to_string(),
            voter: AgentId::from_string(voter),
            approved,
            reasoning: reasoning.to_string(),
            confidence,
        };
        self.inner.lock().submit_vote(response);
    }

    /// 获取投票结果
    ///
    /// Args:
    ///     proposal_id: 提案 ID
    ///
    /// Returns:
    ///     VoteResult or None if no result yet
    fn get_vote_result(&self, proposal_id: &str) -> Option<PyVoteResult> {
        let inner = self.inner.lock();
        let votes = inner.consensus().get_votes(proposal_id)?;
        let _proposal = inner.consensus().get_proposal(proposal_id)?;

        // 统计投票
        let approve = votes.iter().filter(|v| v.approved).count();
        let reject = votes.iter().filter(|v| !v.approved).count();

        // 检查是否达到法定人数（简化：>= 2 票）
        if votes.len() < 2 {
            return None;
        }

        Some(PyVoteResult {
            inner: RustVoteResult {
                proposal_id: proposal_id.to_string(),
                passed: approve > reject,
                approve_count: approve,
                reject_count: reject,
                abstain_count: 0,
            },
        })
    }

    /// 清理已完成的投票
    fn cleanup_vote(&self, proposal_id: &str) {
        self.inner.lock().cleanup_vote(proposal_id);
    }

    fn __repr__(&self) -> String {
        format!(
            "SwarmOrchestrator(agents={})",
            self.inner.lock().agent_count()
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 模块注册
// ═══════════════════════════════════════════════════════════════════════════

/// 注册 swarm 子模块
pub fn register_swarm_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyAgentRole>()?;
    m.add_class::<PyAgentStatus>()?;
    m.add_class::<PyVoteType>()?;
    m.add_class::<PySignalType>()?;
    m.add_class::<PySwarmConfig>()?;
    m.add_class::<PyVoteProposal>()?;
    m.add_class::<PyVoteResult>()?;
    m.add_class::<PyMarketSignal>()?;
    m.add_class::<PySwarmOrchestrator>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_py_agent_role_conversion() {
        assert_eq!(
            RustAgentRole::from(PyAgentRole::Market),
            RustAgentRole::Market
        );
        assert_eq!(RustAgentRole::from(PyAgentRole::Risk), RustAgentRole::Risk);
        assert_eq!(
            RustAgentRole::from(PyAgentRole::Execution),
            RustAgentRole::Execution
        );
        assert_eq!(
            RustAgentRole::from(PyAgentRole::Audit),
            RustAgentRole::Audit
        );
    }

    #[test]
    fn test_py_vote_type_conversion() {
        assert_eq!(
            RustVoteType::from(PyVoteType::TradeDecision),
            RustVoteType::TradeDecision
        );
        assert_eq!(
            RustVoteType::from(PyVoteType::EmergencyStop),
            RustVoteType::EmergencyStop
        );
    }

    #[test]
    fn test_py_signal_type_conversion() {
        assert_eq!(RustSignalType::from(PySignalType::Buy), RustSignalType::Buy);
        assert_eq!(
            RustSignalType::from(PySignalType::Sell),
            RustSignalType::Sell
        );
        assert_eq!(
            RustSignalType::from(PySignalType::Hold),
            RustSignalType::Hold
        );
    }
}
