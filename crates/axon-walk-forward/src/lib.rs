//! AXON 滚动前向验证
//!
//! 提供完整的时间序列验证工具链：Rolling / Expanding 窗口分割、
//! purge / embargo 防泄漏、OOS 指标聚合、Deflated Sharpe Ratio 等。
//!
//! # 模块规划
//!
//! | 模块 | 说明 |
//! |------|------|
//! | [`config`] | WalkForwardConfig + WindowType |
//! | [`split`] | TimeSeriesSplitter（Rolling / Expanding）|
//! | [`purge`] | purge_overlapping_labels / embargo_indices / detect_leakage |
//! | [`metrics`] | FoldResult / ISMetrics / OOSMetrics / WalkForwardResult |
//! | [`evaluation`] | aggregate_folds / deflated_sharpe |
//! | [`error`] | 统一错误类型 |
//! | [`python`] | PyO3 绑定（feature = `python`） |

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod config;
pub mod error;
pub mod evaluation;
pub mod metrics;
pub mod purge;
pub mod split;

#[cfg(feature = "python")]
pub mod python;

pub use config::{WalkForwardConfig, WindowType};
pub use error::{WalkForwardError, WalkForwardResult as WalkForwardErrorResult};
pub use evaluation::{aggregate_folds, compute_deflated_sharpe};
pub use metrics::{
    AggregatedMetrics, FoldResult, FoldSplit, ISMetrics, LeakageCheck, OOSMetrics,
    StabilityMetrics, WalkForwardResult,
};
pub use purge::{detect_leakage, embargo_indices, purge_overlapping_labels};
pub use split::{TimeSeriesSplitter, expand_window, rolling_window};
