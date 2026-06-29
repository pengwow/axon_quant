//! Harness 编排系统 Python 绑定
//!
//! 暴露 PyHarnessBridge / PyCircuitBreaker / PyAuditChain / PyPositionGuard 到 Python。

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

/// Python 可用的 Harness 桥接器
#[pyclass(name = "HarnessBridge")]
pub struct PyHarnessBridge {
    inner: axon_harness::HarnessBridge,
}

#[pymethods]
impl PyHarnessBridge {
    /// 构造零侵入模式的 HarnessBridge
    #[staticmethod]
    fn none() -> Self {
        Self {
            inner: axon_harness::HarnessBridge::none(),
        }
    }

    /// 使用默认组件构造 HarnessBridge
    #[staticmethod]
    fn with_defaults() -> Self {
        let config = axon_harness::HarnessConfig::default();
        Self {
            inner: axon_harness::HarnessBridge::with_defaults(config),
        }
    }

    /// 是否激活（至少有一个组件）
    fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    /// 是否已熔断
    fn is_circuit_break(&self) -> bool {
        self.inner.is_circuit_break()
    }

    /// 消耗 Token，返回 BudgetZone 字符串
    fn consume_tokens(&self, tokens: u64, model: &str) -> String {
        match self.inner.consume_tokens(tokens, model) {
            axon_harness::BudgetZone::Green => "green".into(),
            axon_harness::BudgetZone::Yellow => "yellow".into(),
            axon_harness::BudgetZone::Red => "red".into(),
            axon_harness::BudgetZone::CircuitBreak => "circuit_break".into(),
        }
    }

    /// 获取预算快照（无 Harness 时返回 None）
    fn budget_snapshot<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        Ok(self.inner.budget_snapshot().map(|state| {
            let dict = PyDict::new(py);
            dict.set_item("total_budget", state.total_budget).unwrap();
            dict.set_item("tokens_used", state.tokens_used).unwrap();
            let zone = match state.zone {
                axon_harness::BudgetZone::Green => "green",
                axon_harness::BudgetZone::Yellow => "yellow",
                axon_harness::BudgetZone::Red => "red",
                axon_harness::BudgetZone::CircuitBreak => "circuit_break",
            };
            dict.set_item("zone", zone).unwrap();
            dict.set_item("cost_usd", state.cost_usd).unwrap();
            dict
        }))
    }

    /// 工具门控检查
    fn check_tool(&self, tool: &str, agent: &str, params: &str) -> String {
        let params_value: serde_json::Value = serde_json::from_str(params).unwrap_or(serde_json::Value::Null);
        match self.inner.check_tool(tool, agent, &params_value) {
            axon_harness::GateResult::Allowed => "allowed".into(),
            axon_harness::GateResult::Denied(reason) => format!("denied: {reason}"),
            axon_harness::GateResult::NeedsApproval => "needs_approval".into(),
        }
    }

    /// 记录工具调用
    fn record_tool_call(&self, tool: &str, agent: &str, params: &str, result: &str) {
        let params_value: serde_json::Value = serde_json::from_str(params).unwrap_or(serde_json::Value::Null);
        self.inner.record_tool_call(tool, agent, &params_value, result);
    }

    /// 裁决 Agent 意图
    fn adjudicate(&self, intent: &str, ctx: &str) -> String {
        let intent_value: serde_json::Value = serde_json::from_str(intent).unwrap_or(serde_json::Value::Null);
        let ctx_value: serde_json::Value = serde_json::from_str(ctx).unwrap_or(serde_json::Value::Null);
        
        // 简化实现：将 JSON 转换为 Rust 类型
        let agent_intent = axon_core::harness_types::AgentIntent {
            action: intent_value.get("action").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            tool: intent_value.get("tool").and_then(|v| v.as_str()).map(String::from),
            params: intent_value.get("params").cloned().unwrap_or(serde_json::Value::Null),
            confidence: intent_value.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.5),
            reasoning: intent_value.get("reasoning").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            estimated_tokens: intent_value.get("estimated_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        };
        let task_ctx = axon_core::harness_types::TaskContext {
            step: ctx_value.get("step").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            tokens_used: ctx_value.get("tokens_used").and_then(|v| v.as_u64()).unwrap_or(0),
            task_description: ctx_value.get("task_description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            current_agent: ctx_value.get("current_agent").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            started_at: ctx_value.get("started_at").and_then(|v| v.as_u64()).unwrap_or(0),
            metadata: ctx_value.get("metadata").cloned().unwrap_or(serde_json::Value::Null),
        };
        match self.inner.adjudicate(&agent_intent, &task_ctx) {
            axon_harness::Adjudication::Approved => "approved".into(),
            axon_harness::Adjudication::Rejected(reason) => format!("rejected: {reason}"),
            axon_harness::Adjudication::NeedRevision(feedback) => format!("need_revision: {feedback}"),
            axon_harness::Adjudication::CircuitBreak => "circuit_break".into(),
        }
    }

    /// 检查任务是否可以继续
    fn can_proceed(&self, ctx: &str) -> bool {
        let ctx_value: serde_json::Value = serde_json::from_str(ctx).unwrap_or(serde_json::Value::Null);
        let task_ctx = axon_core::harness_types::TaskContext {
            step: ctx_value.get("step").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            tokens_used: ctx_value.get("tokens_used").and_then(|v| v.as_u64()).unwrap_or(0),
            task_description: ctx_value.get("task_description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            current_agent: ctx_value.get("current_agent").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            started_at: ctx_value.get("started_at").and_then(|v| v.as_u64()).unwrap_or(0),
            metadata: ctx_value.get("metadata").cloned().unwrap_or(serde_json::Value::Null),
        };
        self.inner.can_proceed(&task_ctx)
    }
}

/// Python 可用的熔断器
#[pyclass(name = "CircuitBreaker")]
pub struct PyCircuitBreaker {
    inner: axon_harness::CircuitBreaker,
}

#[pymethods]
impl PyCircuitBreaker {
    /// 创建熔断器（使用默认配置）
    #[new]
    #[pyo3(signature = (max_consecutive_failures=3, cooldown_seconds=60, max_daily_loss_pct=2.0, max_position_pct=20.0, max_daily_trades=100))]
    fn new(
        max_consecutive_failures: u64,
        cooldown_seconds: u64,
        max_daily_loss_pct: f64,
        max_position_pct: f64,
        max_daily_trades: u64,
    ) -> Self {
        let config = axon_harness::CircuitBreakerConfig {
            max_consecutive_failures,
            cooldown_seconds,
            max_daily_loss_pct,
            max_position_pct,
            max_daily_trades,
        };
        Self {
            inner: axon_harness::CircuitBreaker::new(config),
        }
    }

    /// 检查是否允许交易（热路径 < 20ns）
    fn check(&self) -> bool {
        self.inner.check()
    }

    /// 当前状态字符串
    fn state(&self) -> &str {
        match self.inner.state() {
            axon_harness::BreakerState::Closed => "closed",
            axon_harness::BreakerState::Open => "open",
            axon_harness::BreakerState::HalfOpen => "half_open",
        }
    }

    /// 是否熔断中
    fn is_open(&self) -> bool {
        self.inner.is_open()
    }

    /// 记录交易结果
    fn record_trade(&self, pnl: f64, symbol: &str, position_pct: f64) {
        self.inner.record_trade(pnl, symbol, position_pct);
    }

    /// 记录失败
    fn record_failure(&self, reason: &str) {
        self.inner.record_failure(reason);
    }

    /// 强制重置
    fn force_reset(&self) {
        self.inner.force_reset();
    }
}

/// Python 可用的审计链
#[pyclass(name = "AuditChain")]
pub struct PyAuditChain {
    inner: axon_harness::AuditChain,
}

#[pymethods]
impl PyAuditChain {
    /// 创建空审计链
    #[new]
    fn new() -> Self {
        Self {
            inner: axon_harness::AuditChain::new(),
        }
    }

    /// 记录事件，返回 entry_id
    fn record(&mut self, event_type: &str, agent_id: &str, action: &str, details: &str) -> u64 {
        self.inner.record(event_type, agent_id, action, details)
    }

    /// 验证整条链的完整性
    fn verify_chain(&self) -> bool {
        self.inner.verify_chain()
    }

    /// 条目数
    fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }

    /// 最近 N 条（返回字典列表）
    fn recent_entries<'py>(&self, py: Python<'py>, n: usize) -> PyResult<Bound<'py, PyList>> {
        let entries = self.inner.recent_entries(n);
        let list = PyList::empty(py);
        for e in entries {
            let dict = PyDict::new(py);
            dict.set_item("entry_id", e.entry_id)?;
            dict.set_item("timestamp", e.timestamp)?;
            dict.set_item("event_type", &e.event_type)?;
            dict.set_item("agent_id", &e.agent_id)?;
            dict.set_item("action", &e.action)?;
            list.append(dict)?;
        }
        Ok(list)
    }
}

/// 注册 harness 子模块
pub fn register_harness_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyHarnessBridge>()?;
    m.add_class::<PyCircuitBreaker>()?;
    m.add_class::<PyAuditChain>()?;
    Ok(())
}
