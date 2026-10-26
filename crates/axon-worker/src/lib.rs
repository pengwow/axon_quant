//! 策略编排 + 全 Rust 原生回测运行器
//!
//! axon-strategy 定义"策略是什么"，axon-backtest 定义"撮合引擎"，
//! axon-worker 定义"怎么把策略装到引擎上跑一遍"。

pub mod backtest;

pub use backtest::*;

/// `Strategy` → 内核 `StrategyHost` 桥接（`BarContextSim`）
pub mod kernel_host;

/// 实时数据泵：交易所 WS 消息 → 内核事件（去重 + 缺口检测）
pub mod pump;

pub use pump::{Gap, KernelPump, KernelPumpBatch};

/// M5 live 执行出口：意图 → 同步网关 → 交易所回报（当前只到 gateway 抽象）
pub mod live_sink;

pub use live_sink::{GatewayAck, LiveOrderGateway, LiveSink, RiskCheck, VenueUpdate};

/// async `ExchangeAdapter` → 同步 `LiveOrderGateway` 桥接（M5 testnet）
pub mod exchange_gateway;

pub use exchange_gateway::ExchangeGateway;

/// 实时驱动编排：Kernel + KernelPump + ExchangeGateway 装配状态机（M6 live 接线）
pub mod live_driver;

pub use live_driver::LiveDriver;

/// 纸面驱动：Kernel + KernelPump + PaperSink 装配（M6 paper 接线）
pub mod paper_driver;

pub use paper_driver::{PaperDriver, PaperSnapshot};

/// M5 对账与恢复：内核 / OMS 快照 vs 交易所快照，只报告不改账
pub mod reconcile;

pub use reconcile::{
    CashBreak, LocalSnapshot, PositionBreak, ReconcileConfig, ReconcileReport, VenueSnapshot,
    reconcile, resume_guard,
};

/// M3 Shadow oracle：旧路径与新内核双跑，按类别计数差异并落盘
pub mod shadow;

/// crate 内测试共享的交易所 mock（仅测试编译）
#[cfg(test)]
pub(crate) mod test_support;

#[cfg(feature = "python")]
pub mod python;

/// tracing → Python logging 桥（python 绑定）：`install_rust_logging`
#[cfg(feature = "python")]
pub mod python_logging;
