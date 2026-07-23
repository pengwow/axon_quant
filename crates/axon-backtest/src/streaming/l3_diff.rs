//! 0.9.0 C2.1:`L3Book` 增量 diff + 订阅抽象
//!
//! 设计动机:撮合引擎每 bar / 每 fill 推进时,Book 状态变化需要可观察的增量
//! 推送,用于训练可视化、TensorBoard 记录、自定义监控等。
//!
//! 0.9.0 demo:BacktestEngine::subscribe(Box<dyn L3BookSubscriber>, SubscriberKind)
//! 接受订阅者,触发时机见 SubscriberKind。

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};

use axon_core::types::Instrument;

use crate::matching::l3::book::L3Order;

/// L3Book 增量 diff(0.9.0 C2.1 新增)
///
/// 描述一个时间点上,L3Book 相对于上一个时间点的变化:
/// - `added`:新挂入的订单(全量信息)
/// - `removed`:取消或完全成交的 `order_id`
/// - `modified`:部分成交后剩余的订单(全量信息)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct L3BookDiff {
    /// 哪个 instrument 的 diff
    pub instrument: Instrument,
    /// 新增挂单
    pub added: Vec<L3Order>,
    /// 取消 / 完全成交的 order_id
    pub removed: Vec<u64>,
    /// 部分成交后剩余的订单
    pub modified: Vec<L3Order>,
    /// diff 时间戳(纳秒)
    pub timestamp_ns: i64,
}

impl L3BookDiff {
    /// diff 是否为空(无任何变化)
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.modified.is_empty()
    }

    /// 涉及的总变化数(added + removed + modified)
    pub fn total_count(&self) -> usize {
        self.added.len() + self.removed.len() + self.modified.len()
    }
}

/// 订阅粒度(0.9.0 C2.1 新增)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriberKind {
    /// 仅每 bar 末推 1 次(默认,低频)
    PerBar,
    /// 仅每 fill 发生时推 1 次(高频,适合实时监控)
    PerFill,
    /// 每 bar + 每 fill 都推(总频次最高,适合训练可视化)
    Both,
}

impl Default for SubscriberKind {
    #[allow(clippy::derivable_impls)]
    fn default() -> Self {
        Self::PerBar
    }
}

/// L3Book 订阅者 trait(0.9.0 C2.1 新增)
///
/// 实现方需 `Send + Sync`(`BacktestEngine` 派生 `#[pyclass]`,
/// PyO3 自动要求 `Send + Sync`,所以 trait 也需 `Sync`)。
///
/// 实际使用:Python 端 `PyL3BookSubscriber` 持有 `PyObject` 不自动 `Sync`,
/// 通过 `unsafe impl Sync` 显式标注,理由是:
/// - 所有 `PyObject` 访问都在 `Python::attach` GIL 内进行
/// - 不会跨线程同时访问(BacktestEngine 单线程持有 subscribers HashMap)
pub trait L3BookSubscriber: Send + Sync {
    /// 收到 diff 时调用
    fn on_diff(&mut self, diff: &L3BookDiff);
}

#[cfg(test)]
#[allow(unused_imports)] // 与 plan T5 step 1 测试 import 列表保持一致;部分导入为预留示例
mod tests {
    use super::*;
    use crate::matching::l3::book::{L3Book, L3Order};
    use axon_core::market::Side;
    use axon_core::types::Price;
    use std::collections::BTreeMap;

    fn empty_diff(instrument: &axon_core::types::Instrument, ts: i64) -> L3BookDiff {
        L3BookDiff {
            instrument: instrument.clone(),
            added: vec![],
            removed: vec![],
            modified: vec![],
            timestamp_ns: ts,
        }
    }

    #[test]
    fn diff_default_is_empty() {
        let instrument = axon_core::types::Instrument::Spot(axon_core::types::SpotInstrument {
            base: axon_core::types::Symbol::from("BTC"),
            quote: axon_core::types::Symbol::from("USDT"),
        });
        let diff = empty_diff(&instrument, 1000);
        assert!(diff.is_empty());
        assert_eq!(diff.added.len(), 0);
        assert_eq!(diff.removed.len(), 0);
        assert_eq!(diff.modified.len(), 0);
    }

    #[test]
    fn diff_total_count_sums_all_fields() {
        let instrument = axon_core::types::Instrument::Spot(axon_core::types::SpotInstrument {
            base: axon_core::types::Symbol::from("BTC"),
            quote: axon_core::types::Symbol::from("USDT"),
        });
        let diff = L3BookDiff {
            instrument,
            added: vec![L3Order {
                order_id: 1,
                side: Side::Buy,
                qty: 1.0,
                timestamp_ns: 0,
            }],
            removed: vec![2, 3],
            modified: vec![],
            timestamp_ns: 1000,
        };
        assert_eq!(diff.total_count(), 3);
        assert!(!diff.is_empty());
    }

    #[test]
    fn subscriber_kind_default_is_per_bar() {
        assert_eq!(SubscriberKind::default(), SubscriberKind::PerBar);
    }
}
