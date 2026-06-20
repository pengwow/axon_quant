//! AXON 回测引擎
//!
//! 提供事件驱动的回测能力，支持多级撮合（L1/L2/L3）+ 市场冲击集成。
//!
//! # 模块规划
//!
//! | 模块 | 阶段 | 说明 |
//! |------|------|------|
//! | [`engine`] | Phase 1A | 回测引擎主循环 |
//! | [`matching`] | Phase 1A | L1/L2 撮合（价格-时间优先 + 修改/统计） |
//! | [`impact`] | Phase 4 P4.3 | 市场冲击感知撮合（叠加 `ImpactModel`） |

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod engine;
pub mod impact;
pub mod matching;
/// 流式回测引擎
pub mod streaming;
// Stage 2: PyO3 Python 绑定(`axon_quant.backtest` 子模块入口)
// 注:axon-backtest 不依赖 axon-python(避免 cargo 循环依赖),
// 异常基类使用 builtin `PyException`,Python 端 `except Exception` 统一捕获,
// 设计见 `.axon-internal/specs/2026-06-19-python-bindings-expansion-design.md` §3.1.6。
#[cfg(feature = "python")]
pub mod python;

pub use engine::BacktestEngine;
pub use impact::{ImpactStats, ImpactedEngineConfig, ImpactedMatchingEngine};
pub use matching::{
    L1MatchingEngine, L2MatchingEngine, MatchFill, MatchingEngine, MatchingError, MatchingStats,
    OrderAmend, OrderBookEntry, OrderLocation, SubmitResult, build_limit_order,
};
