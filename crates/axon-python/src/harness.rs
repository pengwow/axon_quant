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
}

/// Python 可用的熔断器
#[pyclass(name = "CircuitBreaker")]
pub struct PyCircuitBreaker {
    inner: axon_safety::CircuitBreaker,
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
        let config = axon_safety::CircuitBreakerConfig {
            max_consecutive_failures,
            cooldown_seconds,
            max_daily_loss_pct,
            max_position_pct,
            max_daily_trades,
        };
        Self {
            inner: axon_safety::CircuitBreaker::new(config),
        }
    }

    /// 检查是否允许交易（热路径 < 20ns）
    fn check(&self) -> bool {
        self.inner.check()
    }

    /// 当前状态字符串
    fn state(&self) -> &str {
        match self.inner.state() {
            axon_safety::circuit_breaker::BreakerState::Closed => "closed",
            axon_safety::circuit_breaker::BreakerState::Open => "open",
            axon_safety::circuit_breaker::BreakerState::HalfOpen => "half_open",
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
    inner: axon_safety::AuditChain,
}

#[pymethods]
impl PyAuditChain {
    /// 创建空审计链
    #[new]
    fn new() -> Self {
        Self {
            inner: axon_safety::AuditChain::new(),
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
