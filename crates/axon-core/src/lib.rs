//! AXON 核心类型库
//!
//! 本 crate 是 AXON 工作区的基础依赖，定义整个系统共享的数据类型与错误约定。
//! **必须** 不依赖任何其他 axon-* crate。
//!
//! # 模块
//!
//! | 模块 | 阶段 | 说明 |
//! |------|------|------|
//! | [`time`] | Phase 1A | 时间戳、单调时钟、精度枚举 |
//! | [`types`] | Phase 1A | 通用类型（Price/Quantity/Symbol） |
//! | [`market`] | Phase 1A | 市场数据（Tick/Bar/OrderBook/Trade） |
//! | [`order`] | Phase 1A | 订单类型系统（Order/OrderType/TimeInForce/状态机） |
//! | [`event`] | Phase 1A | 事件系统（Event/EventBuilder/EventRouter） |
//! | [`queue`] | Phase 1A | 事件队列（按时间戳排序/快进/暂停/重放） |
//! | [`portfolio`] | Phase 1A | 投资组合（多币种/多资产/盈亏/净值） |
//! | [`scheduler`] | Phase 1A | 调度器（模拟时钟/定时任务/周期任务/事件循环） |
//! | [`impact`] | Phase 1A P2 | 市场冲击模型（线性/幂律/自适应/Almgren-Chriss） |
//! | [`latency`] | Phase 1A P2 | 延迟模型（固定/正态/指数/均匀/队列/组合） |
//! | [`volatility`] | Phase 4 | 历史波动率估计器（EWMA/滚动/Garman-Klass） |
//! | [`error`] | Phase 0+ | 统一错误类型 |
//!
//! # 设计原则
//!
//! - **零依赖**：除已声明的 workspace 依赖外，不引入新 crate
//! - **稳定优先**：所有公开 API 在 1.0 前允许破坏性变更，但需 ADR 记录
//! - **serde 兼容**：所有跨边界数据可序列化

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod error;
pub mod event;
pub mod fee;
pub mod impact;
pub mod latency;
pub mod market;
pub mod metrics;
pub mod order;
pub mod portfolio;
pub mod queue;
pub mod scheduler;
pub mod time;
pub mod types;
pub mod volatility;

/// Harness 编排系统核心类型（AgentIntent / TaskContext / HarnessResult）
pub mod harness_types;

/// Python 绑定工具宏（py_exception! / parse_py_enum! / dict_field!）
#[cfg(feature = "python-utils")]
pub mod python_utils;

/// SIMD 加速模块（使用 unsafe SIMD intrinsics）
#[allow(unsafe_code)]
pub mod simd;

pub use error::{Error, Result};

// 市场数据核心类型 re-export（便于 `axon_core::Tick` 等短路径）
pub use market::{
    Bar, BarPeriod, MarketDataError, MarketDataResult, OrderBookLevel, OrderBookSnapshot, Side,
    Tick, Trade,
};

// 通用类型 re-export
pub use types::{Price, Quantity, Symbol};

// 时间类型 re-export
pub use time::{MonotonicClock, TimePrecision, Timestamp};

// 交易指标 re-export
pub use metrics::TradingMetrics;

// 订单类型 re-export
pub use order::{
    Order, OrderError, OrderId, OrderResult, OrderStatus, OrderType, RejectReason, TimeInForce,
};

// 事件类型 re-export
pub use event::{
    Event, EventBuilder, EventCollector, EventError, EventHandler, EventResult, EventRouter,
    EventType, FillEvent, MarketDataEvent, MarketDataPayload, OrderAction, OrderEvent,
    SystemAction, SystemEvent,
};

// 事件队列 re-export
pub use queue::{
    EventQueue, EventQueueError, EventQueueResult, QueueMode, QueueStats, QueuedEvent,
};

// 投资组合 re-export
pub use portfolio::{
    Currency, Portfolio, PortfolioError, PortfolioResult, PortfolioSnapshot, Position, TradeRecord,
};

// 调度器 re-export
pub use scheduler::{
    ClosureCallback, RepeatPolicy, Scheduler, SchedulerContext, SchedulerError, SchedulerResult,
    SchedulerStats, SimulatedClock, Task, TaskCallback, TaskId, TaskStatus,
};

// 冲击模型 re-export
pub use impact::{
    AdaptiveImpactModel, AlmgrenChrissModel, ExecutionPlan, ExecutionStep, Impact, ImpactModel,
    ImpactModelConfig, ImpactModelError, ImpactModelResult, LinearImpactModel, PowerLawImpactModel,
    create_model, linear_impact, sqrt_impact,
};

// 延迟模型 re-export
pub use latency::{
    CompositeLatencyModel, ConstantLatencyModel, ExponentialLatencyModel, LatencyModel,
    LatencyModelError, LatencyModelFactory, LatencyModelResult, LatencyParams, NormalLatencyModel,
    PathType, QueueLatencyModel, UniformLatencyModel,
};

// 费用模型 re-export
pub use fee::{
    ExchangeId, FeeBreakdown, FeeModel, FeeModelError, FeeModelResult, FeePosition, FeeRecord,
    FeeTable, FeeTrade, FeeType, TieredFeeModel, TradeRole, VolumeTier,
};

// 波动率估计器 re-export
pub use volatility::{
    EwmaVolatility, GarmanKlassVolatility, OhlcBar, RollingVolatility, VolatilityError,
    VolatilityEstimator, VolatilityResult, VolatilitySource,
};
