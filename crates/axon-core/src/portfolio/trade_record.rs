//! 交易记录

use serde::{Deserialize, Serialize};

use crate::market::Trade;
use crate::types::Instrument;

/// 交易记录（用于审计和盈亏计算）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeRecord {
    /// 成交记录
    pub trade: Trade,
    /// 已实现盈亏（单位：f64 × 1e6）
    pub realized_pnl: i64,
    /// 佣金（单位：f64 × 1e6）
    pub commission: i64,
    /// 净数量（带方向符号）
    pub net_quantity: i64,
    /// T2.4 新增:成交所属的 Instrument(spot/swap)
    ///
    /// 为何单独持有而非从 `Trade` 派生:
    /// - `Trade` 固定 40 字节(`#[repr(C)]`),hot path 必须保持紧凑
    /// - `TradeRecord` 走 `trades: Vec<TradeRecord>` 审计路径,每根 bar 最多
    ///   几次 push,允许额外字段
    /// - 后期 report / PnL 拆分(per-instrument 累计)直接用此字段
    #[serde(default)]
    pub instrument: Instrument,
}

impl TradeRecord {
    /// 创建新交易记录
    ///
    /// # 参数
    ///
    /// - `trade`:成交记录(40 字节紧凑表示)
    /// - `realized_pnl`:已实现盈亏(单位 f64 × 1e6)
    /// - `commission`:佣金(单位 f64 × 1e6)
    /// - `net_quantity`:净数量(带方向符号,f64 × 1e6)
    /// - `instrument`:T2.4 新增,成交所属的 Instrument
    pub fn new(
        trade: Trade,
        realized_pnl: i64,
        commission: i64,
        net_quantity: i64,
        instrument: Instrument,
    ) -> Self {
        Self {
            trade,
            realized_pnl,
            commission,
            net_quantity,
            instrument,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Timestamp;
    use crate::types::{Price, Quantity, SpotInstrument, Symbol};

    fn btc_usdt() -> Instrument {
        Instrument::Spot(SpotInstrument {
            base: Symbol::from("BTC"),
            quote: Symbol::from("USDT"),
        })
    }

    #[test]
    fn test_trade_record_creation() {
        let trade = Trade::new(
            Timestamp::from_nanos(1_000),
            Price::from_f64(100.0),
            Quantity::from_f64(1.0),
            1,
            2,
        );
        let rec = TradeRecord::new(trade, 1_000_000, 100_000, 1_000_000, btc_usdt());
        assert_eq!(rec.realized_pnl, 1_000_000);
        assert_eq!(rec.commission, 100_000);
        assert_eq!(rec.instrument, btc_usdt());
    }
}
