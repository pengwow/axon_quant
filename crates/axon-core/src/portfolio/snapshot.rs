//! 投资组合快照(用于时间序列记录)

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::currency::Currency;
use super::position::Position;
use crate::time::Timestamp;
use crate::types::Instrument;

/// 投资组合快照
///
/// 用于:净值曲线、回放、状态持久化
///
/// **0.5.0 BREAKING**:`positions` 键类型从 `Symbol` 迁到 `Instrument`,
/// 与 `Portfolio::positions` 保持一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioSnapshot {
    /// 快照时间戳
    pub timestamp: Timestamp,
    /// 净值(NAV)
    pub nav: i64,
    /// 多币种现金
    pub cash: HashMap<Currency, i64>,
    /// 持仓映射(**0.5.0 BREAKING**:`HashMap<Instrument, Position>`)
    pub positions: HashMap<Instrument, Position>,
    /// 已实现盈亏
    pub realized_pnl: i64,
    /// 未实现盈亏
    pub unrealized_pnl: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_creation() {
        let snap = PortfolioSnapshot {
            timestamp: Timestamp::from_nanos(1_000),
            nav: 100_000_000_000,
            cash: HashMap::new(),
            positions: HashMap::new(),
            realized_pnl: 0,
            unrealized_pnl: 0,
        };
        assert_eq!(snap.nav, 100_000_000_000);
        assert_eq!(snap.timestamp, Timestamp::from_nanos(1_000));
    }
}
