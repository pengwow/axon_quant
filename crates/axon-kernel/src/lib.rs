//! AXON 统一交易内核
//!
//! 回测 / paper / live 三种模式跑 **同一条主回路**：唯一的差异只在装配时的
//! 数据泵（灌 `BarClosed` vs 聚合 tick）与 `ExecutionSink` 实现（回测撮合 vs
//! 交易所），主回路本身没有任何 mode 分支。
//!
//! 设计依据：[`2026-10-23-unified-kernel-design.md`](../../../QuantCell/docs/superpowers/specs/2026-10-23-unified-kernel-design.md)。
//!
//! # 模块
//!
//! | 模块 | 职责 |
//! |------|------|
//! | [`event`] | `BarClosed` 借用视图 + 周期 → 纳秒 / 收市时刻推导 |
//! | [`aggregator`] | [`BarAggregator`]（tick / kline → 收市 `BarClosed` + 去重） |
//! | [`core`] | [`KernelCore`]（`BacktestEngine` 的内核侧代理）+ [`KernelConfig`] |
//! | [`pump`] | [`HistoryPump`](pump::build_frames) 帧构造 + [`BarGridBarrier`] |
//! | [`host`] | [`StrategyHost`] 同步决策接口 + `OrderIntent` / `HostCtx` |
//! | [`sink`] | [`ExecutionSink`] 执行出口（M1: `NullSink` / `BacktestSink`） |
//! | [`journal`] | 事件溯源日志（M0 内存版） |
//! | [`persist`] | `RunManifest`（重演所需的外部输入指纹） |
//! | [`lifecycle`] | per-bar 九步编排（复刻 `BacktestRunner::run()`） |
//! | [`kernel_loop`] | 主回路 `Kernel`（事件调度 + 决策 + 收尾） |
//! | [`rl`] | [`RLStrategyHost`]：RL 推理接入主回路 |
//!
//! # 当前阶段
//!
//! M1 回测闭环:内核直接驱动 `BacktestEngine`，同一输入下与
//! `axon_worker::BacktestRunner::run()` 的 per-bar 语义逐字一致。模块名用
//! `kernel_loop` 而非 `loop`，因为 `loop` 是 Rust 关键字。
//!
//! # 迁移状态（M6）
//!
//! ## 已完成（M0–M5 可离线部分）
//!
//! - M0 骨架：`BarClosed` 事件 + [`StrategyHost`] / [`ExecutionSink`] 等接口 + 空跑主回路；
//! - M1 回测闭环：[`build_frames`] 历史泵 + [`BacktestSink`] + [`BarLifecycle`] 九步编排，
//!   结果与 `BacktestRunner::run()` 逐字段相等；
//! - M1 journal：append-only JSONL + [`RunManifest`] 复现指纹 + 重演校验，
//!   格式版本见 `persist::JOURNAL_FORMAT_VERSION`；
//! - M2 绑定换芯：`axon-worker` 的 `PyKernel`（`run_history` / `replay_from`）；
//! - M3 shadow oracle：`axon-worker::shadow` 新旧双跑、按类别 diff 落盘；
//! - M4 paper：[`PaperSink`]（L1 撮合 + 盘口报价）+ `axon-worker` 的 `pump::KernelPump`；
//! - M4.5 RL：[`RLStrategyHost`] + 训练/推理共用的 `ObservationSpec` / `CostModel`；
//! - M5 live：`axon-worker` 的 `live_sink::LiveSink`（REST 下单 + 回报转事件，仅到网关抽象）。
//!
//! ## 仍保留的 legacy 路径
//!
//! - `axon_worker::python::PyBacktestRunner`：直接走 `axon_worker::BacktestRunner`
//!   的旧编排（M6 不删，只冻结实现），内核路径改用 `PyKernel`；
//! - QuantCell 侧开关 `engine_config['runner']`：默认 `kernel`；`'legacy'` / 旧名
//!   `'rust'` → `PyBacktestRunner`，`'python'` → Python `BacktestLoop` 回退。
//!
//! ## 仍处于废弃中
//!
//! - `axon-backtest::streaming`：由 [`BarAggregator`] + `KernelPump` + [`PaperSink`]
//!   取代，仅文档标注废弃（工作区仍有 legacy 测试使用，故不加 `#[deprecated]`），
//!   下个大版本移除；
//! - `EventDrivenLoop`（`axon-llm::swarm`）：由 `SwarmOrchestrator` 装配 `CompositeHost`
//!   的路径取代。
//!
//! ## 未验证项
//!
//! - 实盘 testnet 的下单 / 撤单 / 成交回报对账；
//! - paper 与回测的 NAV 漂移对账（滑点带内的容忍度尚未标定）；
//! - RL 观测统计量的落盘（训练期统计 vs 推理期统计的一致性尚无文件级证据）。

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod aggregator;
pub mod core;
pub mod event;
pub mod host;
pub mod journal;
pub mod kernel_loop;
pub mod lifecycle;
pub mod persist;
pub mod pump;
pub mod rl;
pub mod sink;

pub use aggregator::BarAggregator;
pub use core::{InstrumentMode, KernelConfig, KernelCore, SeedLiquidity};
pub use event::{BarClosedEvent, close_time, interval_to_nanos};
pub use host::{HostCtx, OrderId, OrderIntent, OrderSpec, StrategyHost};
pub use journal::{Journal, JournalEntry};
pub use kernel_loop::{Kernel, KernelResult};
pub use lifecycle::BarLifecycle;
pub use persist::{ModelArtifact, RunManifest};
pub use pump::{BarGridBarrier, GridFrame, build_frames};
pub use rl::{InferenceBackend, RLStrategyHost};
pub use sink::{BacktestSink, ExecutionSink, NullSink, PaperSink};

/// 内核错误
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    /// 执行出口（submit / on_market / poll / finish_bar）处理失败
    #[error("execution sink failed: {0}")]
    Sink(String),
    /// 历史数据泵 / 网格屏障失败（数据长度不一致、帧乱序、非法周期等）
    #[error("history pump failed: {0}")]
    Pump(String),
    /// journal 落盘 / 读回 / 重演校验失败（信息可读，含文件与行号）
    #[error("journal failed: {0}")]
    Journal(String),
}
