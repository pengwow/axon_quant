//! Harness 编排系统 trait 接口
//!
//! 定义 HarnessPolicy / ToolGate / BudgetGuard 等核心 trait，
//! 以及 HarnessBridge 桥接器。
//!
//! 安全组件（从 axon-safety 合并）：
//! - CircuitBreaker: 熔断器（AtomicU8 状态机，热路径 < 20ns）
//! - AuditChain: 审计链（Blake3 哈希链，防篡改）
//! - PositionGuard: 仓位守卫
//!
//! 核心组件：
//! - DefaultPolicy: 默认裁决策略
//! - SimpleBudgetGuard: Token 预算守卫
//! - RBACToolGate: 基于角色的工具门控
//! - HarnessObserver: 可观测性组件

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod bridge;
pub mod policy;
pub mod types;

// 安全组件（从 axon-safety 合并）
pub mod circuit_breaker;
pub mod audit;
pub mod position;

// 核心组件
pub mod default_policy;
pub mod simple_budget;
pub mod rbac_gate;
pub mod observer;

pub use bridge::HarnessBridge;
pub use policy::{BudgetGuard, HarnessPolicy, ToolGate};
pub use types::*;

// 安全组件导出
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, BreakerState};
pub use audit::{AuditChain, AuditEntry};
pub use position::PositionGuard;

// 核心组件导出
pub use default_policy::DefaultPolicy;
pub use simple_budget::SimpleBudgetGuard;
pub use rbac_gate::RBACToolGate;
pub use observer::{HarnessObserver, DecisionRecord, HarnessMetrics};
