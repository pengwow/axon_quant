//! 撮合引擎集成市场冲击模型
//!
//! 将 [`axon_core::impact`] 中定义的市场冲击模型（`ImpactModel`）应用于
//! [`crate::matching::L1MatchingEngine`] 的撮合流程，使回测能模拟
//! 真实市场中订单对价格的瞬时与永久影响。
//!
//! # 设计动机
//!
//! 真实市场中大单会"吃"订单簿深度，并推动价格移动。无冲击的回测结果
//! 过于乐观——它假设所有订单都能以历史价或更优价成交。
//! `ImpactedMatchingEngine` 在撮合成交价上叠加 [`Impact`](axon_core::impact::Impact) 偏移：
//!
//! - **即时冲击**（`instantaneous`）：影响**本次**成交价
//! - **永久冲击**（`permanent`）：累积到内部状态，影响**后续**订单簿中间价
//!
//! # 流程
//!
//! 1. 接收订单
//! 2. 由内部 `L1MatchingEngine` 生成**裸成交**（不应用冲击）
//! 3. 从当前订单簿（含永久冲击偏移）生成 [`OrderBookSnapshot`](axon_core::market::OrderBookSnapshot)
//! 4. 调用 `ImpactModel::compute_impact()` 计算冲击
//! 5. 将即时冲击叠加到每笔 `MatchFill.price` 上
//! 6. 把永久冲击累加到内部状态（`permanent_offset`）
//!
//! # 永久冲击衰减
//!
//! 真实市场的永久冲击会随时间衰减（流动性恢复、套利者介入等）。
//! 可通过 `with_permanent_decay()` 启用：
//!
//! ```text
//! offset_{n+1} = offset_n × (1 - decay_per_fill) + new_permanent
//! ```
//!
//! # 模块组织
//!
//! - [`impacted_engine`]：核心 [`ImpactedMatchingEngine`] 包装器
//! - [`config`]：TOML 配置文件加载
//!
//! # Python 绑定
//!
//! PyO3 桥接层已迁移到 `crate::python::impact`(Stage 2 Task 7),
//! 详见 `python::impact` 模块说明。

pub mod config;
pub mod impacted_engine;

pub use config::ImpactedEngineConfig;
pub use impacted_engine::{
    ImpactStats, ImpactedMatchingEngine, build_snapshot_from_levels, decay_permanent_offset,
    price_with_impact,
};
