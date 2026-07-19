//! # axon-risk
//!
//! 风控引擎：预交易检查、组合级风险监控、熔断器管理和风险指标计算。
//!
//! ## 核心功能
//!
//! - **预交易检查**：订单提交前的风控验证（仓位限制、杠杆、订单大小）
//! - **熔断器**：连续亏损自动暂停交易，冷却期后自动恢复
//! - **组合监控**：集中度检查、回撤监控、每日 PnL 追踪
//! - **风险指标**：VaR（历史模拟法）、杠杆率、持仓集中度
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! use axon_risk::{DefaultRiskEngine, RiskConfig, RiskEngine, RiskResult};
//!
//! // 创建风控引擎
//! let engine = DefaultRiskEngine::new(RiskConfig::default());
//!
//! // 检查订单
//! // let result = engine.check_order(&order, &portfolio);
//! // match result {
//! //     RiskResult::Allow => { /* 提交订单 */ }
//! //     RiskResult::Reject(reason) => { /* 拒绝订单 */ }
//! //     RiskResult::Warn(msg) => { /* 警告但允许 */ }
//! // }
//! ```
//!
//! ## 架构
//!
//! ```text
//! check_order
//!     │
//!     ├─→ 熔断器检查 (AtomicBool, ~5ns)
//!     ├─→ 订单大小检查 (~10ns)
//!     ├─→ 仓位限制检查 (~50ns)
//!     ├─→ 杠杆检查 (~20ns)
//!     └─→ 回撤检查 (~20ns)
//! ```
//!
//! ## 性能
//!
//! | 操作 | 延迟 |
//! |------|------|
//! | check_order | 12ns |
//! | update_daily_pnl | 5ns |
//! | get_metrics | 13ns |

pub mod checks;
pub mod circuit_breaker;
pub mod config;
pub mod engine;
pub mod error;
pub mod handler;
pub mod metrics;
pub mod portfolio;
pub mod utils;

// Stage 3:`axon-risk` 的 PyO3 Python 绑定(`axon_quant.risk` 子模块)。
//
// 设计:`RiskError` 继承 builtin `PyException`(不引 `axon_python::AxonError`),
// 避免 `axon-risk` 反向依赖 `axon-python` 造成 cargo 循环(Stage 1/2 实战发现)。
// 详见 `crates/axon-risk/src/python/mod.rs` 注释 + `.axon-internal/plans/
// 2026-06-19-python-bindings-expansion-s3-axon-risk.md`。
#[cfg(feature = "python")]
pub mod python;

pub use config::RiskConfig;
pub use engine::{DefaultRiskEngine, RiskEngine};
pub use error::{AlertSeverity, RiskAlert, RiskError, RiskReason, RiskResult};
pub use handler::RiskEventHandler;
pub use metrics::RiskMetrics;
