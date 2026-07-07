//! Swarm 模块 Python 绑定(0.3.0 P0 T2.9)
//!
//! 暴露以下 pyclass / pyfunction:
//! - `SwarmConfig` / `AgentRole` / `AgentStatus` / `VoteType` / `SignalType`
//! - `MarketSignal` / `VoteProposal` / `VoteResult`
//! - `SwarmOrchestrator`:start / stop / inject_* / stats / register_*_agent
//! - `TradingTools`:`ExecutionAgent` 的工具集合(place_order + query_portfolio)
//!
//! Python 端典型用法:
//! ```python
//! from axon_quant._native.llm import trading, swarm
//!
//! config = swarm.SwarmConfig(vote_timeout_ms=5000)
//! orch = swarm.SwarmOrchestrator(config)
//!
//! # 创建 4 agent(Market/Risk/Audit 零配置;Execution 需要 tools)
//! tools = swarm.TradingTools(place_order=place, query_portfolio=query)
//! orch.register_market_agent(agent_id="m0", symbols=["BTC-USDT"])
//! orch.register_risk_agent(agent_id="r0")
//! orch.register_execution_agent(agent_id="e0", tools=tools)
//! orch.register_audit_agent(agent_id="a0")
//!
//! orch.start()
//! orch.inject_market_signal(swarm.MarketSignal(
//!     symbol="BTC-USDT", signal_type=swarm.SignalType.Buy,
//!     confidence=0.9, reasoning="...",
//! ))
//! import time; time.sleep(0.5)
//! print(orch.stats())
//! orch.stop()
//! ```

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]

use std::sync::Arc as StdArc;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use tokio::sync::mpsc;
use tokio::sync::Mutex as TokioMutex;

use crate::swarm::agent::{AgentId, AgentRole as RustAgentRole, AgentStatus as RustAgentStatus};
use crate::swarm::agents::execution_agent::{
    ExecutionAgent, ExecutionAgentConfig, TradingTools as RustTradingTools,
};
use crate::swarm::agents::market_agent::{MarketAgent, MarketAgentConfig};
use crate::swarm::market_data::MockSourceAdapter;
use crate::swarm::message::{
    AgentMessage, MarketSignal as RustMarketSignal, MessageContent, SignalType as RustSignalType,
    VoteProposal as RustVoteProposal, VoteResult as RustVoteResult, VoteType as RustVoteType,
};
use crate::swarm::orchestrator::{
    AgentHandle, SwarmConfig as RustSwarmConfig, SwarmOrchestrator,
};
use crate::swarm::vote::VoteResponse;

use super::trading::{PyPlaceOrderTool, PyQueryPortfolioTool};

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
    #[pyo3(signature = (vote_timeout_ms=5000, loop_tick_ms=100))]
    fn new(vote_timeout_ms: u64, loop_tick_ms: u64) -> Self {
        Self {
            inner: RustSwarmConfig {
                vote_timeout_ms,
                loop_tick_ms,
                ..Default::default()
            },
        }
    }

    /// 获取 vote_timeout_ms
    #[getter]
    fn vote_timeout_ms(&self) -> u64 {
        self.inner.vote_timeout_ms
    }

    /// 获取 loop_tick_ms
    #[getter]
    fn loop_tick_ms(&self) -> u64 {
        self.inner.loop_tick_ms
    }

    fn __repr__(&self) -> String {
        format!(
            "SwarmConfig(vote_timeout_ms={}, loop_tick_ms={})",
            self.inner.vote_timeout_ms, self.inner.loop_tick_ms
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

/// ExecutionAgent 工具集合(0.3.0 P0 T2.8 配套)
#[pyclass(name = "TradingTools", from_py_object)]
#[derive(Clone)]
pub struct PyTradingTools {
    inner: RustTradingTools,
}

#[pymethods]
impl PyTradingTools {
    #[new]
    fn new(place_order: &PyPlaceOrderTool, query_portfolio: &PyQueryPortfolioTool) -> Self {
        Self {
            inner: RustTradingTools::new(place_order.tool.clone(), query_portfolio.tool.clone()),
        }
    }

    fn __repr__(&self) -> String {
        "TradingTools(place_order=..., query_portfolio=...)".to_string()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// SwarmOrchestrator
// ═══════════════════════════════════════════════════════════════════════════

/// 内部状态:在 start() 之后持有 inject sender + JoinHandle
struct OrchRuntime {
    /// 向 run_loop_arc inbox 投递消息的 sender
    inject_tx: mpsc::Sender<AgentMessage>,
    /// run_loop_arc 的 JoinHandle
    handle: tokio::task::JoinHandle<()>,
}

/// Swarm 编排器 — Agent 生命周期管理、消息路由、投票共识
///
/// Python 端使用流程:
/// 1. 构造 `SwarmOrchestrator(config)`
/// 2. 注册 4 类 agent(`register_market_agent` / `register_risk_agent` /
///    `register_execution_agent` / `register_audit_agent`)
/// 3. `start()` 启动 `run_loop_arc` 后台 task
/// 4. `inject_market_signal(...)` / `inject_vote_response(...)` 等投递消息
/// 5. `stats()` 读取统计;`stop()` 关闭
#[pyclass(name = "SwarmOrchestrator")]
pub struct PySwarmOrchestrator {
    /// 共享的 orchestrator(Arc<Mutex<...>>)
    inner: StdArc<TokioMutex<SwarmOrchestrator>>,
    /// Owned tokio runtime(为避免与 orchestrator 内部 tokio 冲突,独占一个)
    runtime: StdArc<tokio::runtime::Runtime>,
    /// start() 之后激活;None 表示未启动或已 stop
    runtime_state: parking_lot::Mutex<Option<OrchRuntime>>,
}

#[pymethods]
impl PySwarmOrchestrator {
    #[new]
    fn new(config: &PySwarmConfig) -> PyResult<Self> {
        let (tx, rx) = mpsc::channel(1000);
        let orchestrator = SwarmOrchestrator::with_channels(config.inner.clone(), rx, tx);
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self {
            inner: StdArc::new(TokioMutex::new(orchestrator)),
            runtime: StdArc::new(runtime),
            runtime_state: parking_lot::Mutex::new(None),
        })
    }

    /// 注册旧版 Agent(`AgentHandle` 模式,无 runner / run_step)
    fn register_agent(&self, agent_id: &str, role: PyAgentRole) -> PyResult<()> {
        let (agent_tx, _agent_rx) = mpsc::channel(100);
        let handle = AgentHandle {
            id: AgentId::from_string(agent_id),
            role: role.into(),
            status: RustAgentStatus::Idle,
            sender: agent_tx,
        };
        self.runtime
            .block_on(async {
                let mut g = self.inner.lock().await;
                g.register_agent(handle)
            })
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
    }

    /// 注销 Agent
    fn unregister_agent(&self, agent_id: &str) -> bool {
        self.runtime.block_on(async {
            let mut g = self.inner.lock().await;
            g.unregister_agent(&AgentId::from_string(agent_id)).is_some()
        })
    }

    /// Agent 总数
    fn agent_count(&self) -> usize {
        self.runtime.block_on(async {
            let g = self.inner.lock().await;
            g.agent_count()
        })
    }

    /// 指定角色的 Agent 数
    fn agent_count_by_role(&self, role: PyAgentRole) -> usize {
        self.runtime.block_on(async {
            let g = self.inner.lock().await;
            g.agent_count_by_role(role.into())
        })
    }

    /// Agent 状态查询
    fn agent_status(&self, agent_id: &str) -> Option<PyAgentStatus> {
        self.runtime.block_on(async {
            let g = self.inner.lock().await;
            g.agent_status(&AgentId::from_string(agent_id))
                .map(PyAgentStatus::from)
        })
    }

    // ═══════════════════════════════════════════════════════════════════
    // 0.3.0 P0 T2.9 新增:4 类 agent 的便捷注册
    // ═══════════════════════════════════════════════════════════════════

    /// 注册 MarketAgent(零数据源配置)
    ///
    /// Args:
    ///     agent_id: Agent 唯一 ID
    ///     symbols: 关注交易对列表(默认 `["BTC-USDT"]`)
    ///     price_change_threshold: 信号阈值(默认 0.7)
    #[pyo3(signature = (agent_id, symbols=None, price_change_threshold=None))]
    fn register_market_agent(
        &self,
        agent_id: &str,
        symbols: Option<Vec<String>>,
        price_change_threshold: Option<f64>,
    ) -> PyResult<()> {
        let symbols = symbols.unwrap_or_else(|| vec!["BTC-USDT".to_string()]);
        let threshold = price_change_threshold.unwrap_or(0.7);
        // 构造 MarketAgent + Mock 数据源(空 ticks,等待外部 tick 注入)
        let cfg = MarketAgentConfig {
            symbols: symbols.clone(),
            signal_threshold: threshold,
        };
        let (inbox_tx, inbox_rx) = mpsc::channel::<AgentMessage>(64);
        let (outbox_tx, mut outbox_rx) = mpsc::channel::<AgentMessage>(64);
        // 挂载 mock 数据源(后续可通过 attach_data_source 替换)
        let data = MockSourceAdapter::from_ticks(
            format!("{}_data", agent_id),
            vec![],
        );
        let agent = MarketAgent::with_data_source(
            AgentId::from_string(agent_id),
            cfg,
            inbox_rx,
            outbox_tx,
            Box::new(data),
        );
        // 把 agent 包装成 runner 注册到 orchestrator
        let runner: StdArc<dyn crate::swarm::agent_runner::DeclarativeAgentRunner> =
            StdArc::new(agent);
        // Lazy start:首次 register 时自动启动 run_loop
        self.ensure_runtime()?;
        let orch_inbox_tx = {
            let g = self.runtime_state.lock();
            g.as_ref()
                .map(|r| r.inject_tx.clone())
                .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                    "SwarmOrchestrator not started",
                ))?
        };
        self.runtime.block_on(async {
            let mut g = self.inner.lock().await;
            g.register_agent_runner(runner, inbox_tx, orch_inbox_tx)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
        })?;
        // spawn fan-in: agent outbox → orchestrator inject_tx
        let inject_tx = {
            let g = self.runtime_state.lock();
            g.as_ref().map(|r| r.inject_tx.clone()).unwrap()
        };
        self.runtime.spawn(async move {
            while let Some(msg) = outbox_rx.recv().await {
                if inject_tx.send(msg).await.is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    /// 注册 RiskAgent(基础配置,默认阈值)
    fn register_risk_agent(&self, agent_id: &str) -> PyResult<()> {
        let (inbox_tx, inbox_rx) = mpsc::channel::<AgentMessage>(64);
        let (outbox_tx, mut outbox_rx) = mpsc::channel::<AgentMessage>(64);
        let agent = crate::swarm::agents::risk_agent::RiskAgent::new(
            AgentId::from_string(agent_id),
            crate::swarm::agents::risk_agent::RiskAgentConfig::default(),
            inbox_rx,
            outbox_tx,
        );
        let runner: StdArc<dyn crate::swarm::agent_runner::DeclarativeAgentRunner> =
            StdArc::new(agent);
        self.ensure_runtime()?;
        let orch_inbox_tx = {
            let g = self.runtime_state.lock();
            g.as_ref()
                .map(|r| r.inject_tx.clone())
                .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                    "SwarmOrchestrator not started",
                ))?
        };
        self.runtime.block_on(async {
            let mut g = self.inner.lock().await;
            g.register_agent_runner(runner, inbox_tx, orch_inbox_tx)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
        })?;
        let inject_tx = {
            let g = self.runtime_state.lock();
            g.as_ref().map(|r| r.inject_tx.clone()).unwrap()
        };
        self.runtime.spawn(async move {
            while let Some(msg) = outbox_rx.recv().await {
                if inject_tx.send(msg).await.is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    /// 注册 ExecutionAgent(必须传 tools,否则 agent 是"模拟模式"无 backend)
    #[pyo3(signature = (agent_id, tools=None))]
    fn register_execution_agent(
        &self,
        agent_id: &str,
        tools: Option<&PyTradingTools>,
    ) -> PyResult<()> {
        let (inbox_tx, inbox_rx) = mpsc::channel::<AgentMessage>(64);
        let (outbox_tx, mut outbox_rx) = mpsc::channel::<AgentMessage>(64);
        let cfg = match tools {
            Some(t) => ExecutionAgentConfig::with_tools(t.inner.clone()),
            None => ExecutionAgentConfig::default(),
        };
        let agent = ExecutionAgent::new(
            AgentId::from_string(agent_id),
            cfg,
            inbox_rx,
            outbox_tx,
        );
        let runner: StdArc<dyn crate::swarm::agent_runner::DeclarativeAgentRunner> =
            StdArc::new(agent);
        self.ensure_runtime()?;
        let orch_inbox_tx = {
            let g = self.runtime_state.lock();
            g.as_ref()
                .map(|r| r.inject_tx.clone())
                .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                    "SwarmOrchestrator not started",
                ))?
        };
        self.runtime.block_on(async {
            let mut g = self.inner.lock().await;
            g.register_agent_runner(runner, inbox_tx, orch_inbox_tx)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
        })?;
        let inject_tx = {
            let g = self.runtime_state.lock();
            g.as_ref().map(|r| r.inject_tx.clone()).unwrap()
        };
        self.runtime.spawn(async move {
            while let Some(msg) = outbox_rx.recv().await {
                if inject_tx.send(msg).await.is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    /// 注册 AuditAgent(基础配置)
    fn register_audit_agent(&self, agent_id: &str) -> PyResult<()> {
        let (inbox_tx, inbox_rx) = mpsc::channel::<AgentMessage>(64);
        let (outbox_tx, mut outbox_rx) = mpsc::channel::<AgentMessage>(64);
        let agent = crate::swarm::agents::audit_agent::AuditAgent::new(
            AgentId::from_string(agent_id),
            crate::swarm::agents::audit_agent::AuditAgentConfig::default(),
            inbox_rx,
            outbox_tx,
        );
        let runner: StdArc<dyn crate::swarm::agent_runner::DeclarativeAgentRunner> =
            StdArc::new(agent);
        self.ensure_runtime()?;
        let orch_inbox_tx = {
            let g = self.runtime_state.lock();
            g.as_ref()
                .map(|r| r.inject_tx.clone())
                .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                    "SwarmOrchestrator not started",
                ))?
        };
        self.runtime.block_on(async {
            let mut g = self.inner.lock().await;
            g.register_agent_runner(runner, inbox_tx, orch_inbox_tx)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
        })?;
        let inject_tx = {
            let g = self.runtime_state.lock();
            g.as_ref().map(|r| r.inject_tx.clone()).unwrap()
        };
        self.runtime.spawn(async move {
            while let Some(msg) = outbox_rx.recv().await {
                if inject_tx.send(msg).await.is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    // ═══════════════════════════════════════════════════════════════════
    // 0.3.0 P0 T2.9 新增:start / stop / inject / stats
    // ═══════════════════════════════════════════════════════════════════

    /// 启动 `run_loop_arc` 后台 task
    ///
    /// 调用后:
    /// - orchestrator.run_loop_arc 在独占 tokio runtime 上跑
    /// - `inject_*` 方法可以投递消息
    /// - `stats()` 可读统计
    ///
    /// 已启动时再次调用返回错误。
    fn start(&self) -> PyResult<()> {
        self.ensure_runtime()?;
        Ok(())
    }

    /// 内部:确保 runtime 已启动(register_*_agent 也用)
    fn ensure_runtime(&self) -> PyResult<()> {
        {
            let g = self.runtime_state.lock();
            if g.is_some() {
                return Ok(());
            }
        }
        // 构造 inbox pair(orchestrator 收消息用)
        let (inject_tx, inject_rx) = mpsc::channel::<AgentMessage>(256);
        let orch = StdArc::clone(&self.inner);
        let handle = self.runtime.spawn(async move {
            SwarmOrchestrator::run_loop_arc(orch, inject_rx).await;
        });
        *self.runtime_state.lock() = Some(OrchRuntime { inject_tx, handle });
        Ok(())
    }

    /// 停止 orchestrator(`request_shutdown` + drop inject_tx,让 loop 退出)
    fn stop(&self) -> PyResult<()> {
        let rt = self.runtime_state.lock().take();
        let Some(rt) = rt else {
            return Err(pyo3::exceptions::PyRuntimeError::new_err(
                "SwarmOrchestrator not started",
            ));
        };
        // request_shutdown
        self.runtime.block_on(async {
            let mut g = self.inner.lock().await;
            g.request_shutdown();
        });
        // drop inject_tx 在 rt 离开作用域时自动发生 → run_loop 会退出
        drop(rt.inject_tx);
        // 等 task 结束(2s 超时,避免 Python 卡死)
        let _ = self
            .runtime
            .block_on(async { tokio::time::timeout(std::time::Duration::from_secs(2), rt.handle).await });
        Ok(())
    }

    /// 是否正在运行
    fn is_running(&self) -> bool {
        self.runtime_state.lock().is_some()
    }

    /// 投递 MarketSignal 给 orchestrator(由 `dispatch(MarketAnalysis)` 创建投票)
    fn inject_market_signal(&self, signal: &PyMarketSignal) -> PyResult<()> {
        let tx = {
            let g = self.runtime_state.lock();
            g.as_ref()
                .map(|r| r.inject_tx.clone())
                .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                    "SwarmOrchestrator not started",
                ))?
        };
        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("python"),
            to: AgentId::from_string("orchestrator"),
            correlation_id: None,
            content: MessageContent::MarketAnalysis(signal.inner.clone()),
            timestamp: chrono::Utc::now().timestamp(),
        };
        self.runtime
            .block_on(async { tx.send(msg).await })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// 提交投票响应(Risk / Execution agent 投票)
    fn inject_vote_response(
        &self,
        proposal_id: &str,
        voter: &str,
        approved: bool,
        reasoning: &str,
        confidence: f64,
    ) -> PyResult<()> {
        let tx = {
            let g = self.runtime_state.lock();
            g.as_ref()
                .map(|r| r.inject_tx.clone())
                .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                    "SwarmOrchestrator not started",
                ))?
        };
        let response = VoteResponse {
            proposal_id: proposal_id.to_string(),
            voter: AgentId::from_string(voter),
            approved,
            reasoning: reasoning.to_string(),
            confidence,
        };
        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string(voter),
            to: AgentId::from_string("orchestrator"),
            correlation_id: Some(proposal_id.to_string()),
            content: MessageContent::VoteResponse(crate::swarm::message::VoteResult {
                proposal_id: response.proposal_id.clone(),
                passed: response.approved,
                approve_count: if response.approved { 1 } else { 0 },
                reject_count: if response.approved { 0 } else { 1 },
                abstain_count: 0,
            }),
            timestamp: chrono::Utc::now().timestamp(),
        };
        // 同时把 response 写进 consensus(让 orchestrator 投票统计生效)
        self.runtime.block_on(async {
            let mut g = self.inner.lock().await;
            g.submit_vote(response);
            tx.send(msg).await
        })
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// 触发 Shutdown(stop() 的"软"版本,不 join task)
    fn inject_shutdown(&self) -> PyResult<()> {
        let tx = {
            let g = self.runtime_state.lock();
            g.as_ref()
                .map(|r| r.inject_tx.clone())
                .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                    "SwarmOrchestrator not started",
                ))?
        };
        let msg = AgentMessage {
            id: crate::swarm::message::MessageId::new(),
            from: AgentId::from_string("python"),
            to: AgentId::from_string("orchestrator"),
            correlation_id: None,
            content: MessageContent::Shutdown,
            timestamp: chrono::Utc::now().timestamp(),
        };
        self.runtime
            .block_on(async { tx.send(msg).await })
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// 获取统计 dict(`messages_processed` / `market_signals` / `risk_assessments` /
    /// `execution_results` / `votes_created` / `votes_passed` / `votes_rejected` /
    /// `harness_approved` / `harness_rejected` / `harness_circuit_break` / `shutdowns`)
    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        self.runtime.block_on(async {
            let g = self.inner.lock().await;
            let s = g.stats();
            let d = PyDict::new(py);
            d.set_item("messages_processed", s.messages_processed)?;
            d.set_item("market_signals", s.market_signals)?;
            d.set_item("risk_assessments", s.risk_assessments)?;
            d.set_item("execution_results", s.execution_results)?;
            d.set_item("votes_created", s.votes_created)?;
            d.set_item("votes_passed", s.votes_passed)?;
            d.set_item("votes_rejected", s.votes_rejected)?;
            d.set_item("harness_approved", s.harness_approved)?;
            d.set_item("harness_rejected", s.harness_rejected)?;
            d.set_item("harness_circuit_break", s.harness_circuit_break)?;
            d.set_item("shutdowns", s.shutdowns)?;
            Ok(d)
        })
    }

    /// 发起投票
    fn create_vote(&self, proposal: &PyVoteProposal) -> String {
        self.runtime.block_on(async {
            let mut g = self.inner.lock().await;
            g.create_vote(proposal.inner.clone())
        })
    }

    fn __repr__(&self) -> String {
        let agent_count = self.agent_count();
        let running = self.is_running();
        format!(
            "SwarmOrchestrator(agents={}, running={})",
            agent_count, running
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
    m.add_class::<PyTradingTools>()?;
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
