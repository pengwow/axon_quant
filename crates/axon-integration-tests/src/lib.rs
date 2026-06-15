//! AXON Phase 2 集成测试
//!
//! 本 crate 验证 Phase 2 各模块的端到端协作：
//! - HPO（超参优化）+ Tracker（实验追踪）：超参搜索过程中的实时指标记录
//! - Walk-forward（滚动前向验证）+ Registry（模型注册）：验证后的最佳模型自动注册
//! - Tracker + Registry：根据追踪的指标决策阶段转换（staging → production）
//! - HPO 多目标 + Pareto + Tracker：多目标优化的指标追踪与前沿选择
//! - 端到端训练管线：HPO → Walk-forward → Tracker → Registry 全链路
//!
//! ## 模块规划
//!
//! | 测试模块 | 涉及 crate | 场景 |
//! |---------|-----------|------|
//! | [`hpo_tracker`] | axon-hpo + axon-tracker | 超参搜索 + 指标记录 |
//! | [`walkforward_registry`] | axon-walk-forward + axon-registry | 验证后注册 |
//! | [`tracker_registry`] | axon-tracker + axon-registry | 指标驱动阶段转换 |
//! | [`multi_objective`] | axon-hpo + axon-tracker | Pareto 前沿追踪 |
//! | [`e2e_pipeline`] | 所有 4 个 Phase 2 crate | 端到端训练管线 |
//! | [`error_recovery_and_concurrency`] | 多 crate | 错误恢复 + 并发 |
//! | [`fuzz`] | axon-core + axon-backtest | 属性测试 / 模糊测试 |
//! | [`contract`] | 所有 crate | API/数据契约稳定性 |

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

/// 共享的测试辅助函数与 fixture
pub mod fixtures;

/// 错误恢复与并发场景的集成测试
pub mod error_recovery_and_concurrency;

/// 模糊测试（基于 `proptest` 的 property-based fuzz）
pub mod fuzz;

/// 契约测试（API/数据契约稳定性）
pub mod contract;

pub mod e2e_pipeline;
/// 集成测试模块（按 crate 维度组织）
pub mod hpo_tracker;
pub mod multi_objective;
/// Phase 4 端到端集成测试
pub mod phase4_e2e;
pub mod tracker_registry;
pub mod walkforward_registry;

/// 场景 1：回测引擎撮合全流程
pub mod matching_flow;
/// 场景 3：HPO 超参数优化全流程
pub mod hpo_flow;
/// 场景 4：Walk-Forward 验证全流程
pub mod walkforward_flow;
/// 场景 5：实验追踪全流程
pub mod tracker_registry_flow;
/// 场景 6：分布式训练全流程
pub mod distributed_flow;
